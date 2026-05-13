// Headless WebGPU browser test for the graph-renderer.
//
// Spins up `graph-api` as a subprocess against a synthetic test vault,
// opens the served page in headless Chromium with WebGPU enabled, lets
// the force sim run for a few seconds, and asserts:
//   1. no page errors
//   2. the canvas isn't ~all-black after boot
//   3. a click on the canvas does NOT blank the canvas (regression for
//      the cursor-force-on-LMB bug — commit 595d8641)
//   4. /node/:id round-trips tags from a vault with mixed YAML shapes
//      (regression for `vault-links` parser only accepting arrays —
//      commit b533f1ab)
//   5. /compute/health returns a parseable broker status (regression
//      for renderer never seeing back-half-of-chain liveness — commit
//      1e5a358e)
//
// Output: tests/browser/out/screenshot{,-post-click}.png + a single
// JSON line on stdout. Exit 0 = ok, 1 = any check failed.

import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { platform } from 'node:os';
import { PNG } from 'pngjs';

const REPO_ROOT  = resolve(process.cwd(), '..', '..');
const VAULT      = process.env.VAULT_ROOT || '/tmp/test-vault';
const PORT       = Number(process.env.TEST_PORT || 47895);
const URL        = `http://127.0.0.1:${PORT}/`;
const OUT        = resolve('out');
const ASSETS_DIR = resolve(REPO_ROOT, 'crates/graph-renderer/assets/dist');
const BIN        = resolve(REPO_ROOT, 'target/release/graph-api');

mkdirSync(OUT, { recursive: true });

// ---- 0. seed the test vault with mixed YAML tag shapes -------------------
//
// The smoke vault used to be wikilink-only (`Alpha.md` ↔ `Beta.md` ↔
// `Gamma.md`). We now also write tag-shape fixtures so check #4 below
// can verify every shape survives the parse → proto → /node/:id
// round-trip. Each `Tag*.md` covers one shape that the legacy parser
// silently dropped before commit b533f1ab.
mkdirSync(VAULT, { recursive: true });
writeFileSync(`${VAULT}/Alpha.md`, 'See [[Beta]] and [[Gamma]].\n');
writeFileSync(`${VAULT}/Beta.md`,  '[[Alpha]]\n');
writeFileSync(`${VAULT}/Gamma.md`, '[[Alpha]] [[Beta]]\n');
writeFileSync(
  `${VAULT}/TagArray.md`,
  '---\ntags: [alpha, beta]\n---\n\n[[Alpha]]\n',
);
writeFileSync(
  `${VAULT}/TagCsv.md`,
  '---\ntags: gamma, delta\n---\n\n[[Beta]]\n',
);
writeFileSync(
  `${VAULT}/TagScalar.md`,
  '---\ntags: epsilon\n---\n\n[[Alpha]]\n',
);
writeFileSync(
  `${VAULT}/TagInline.md`,
  '---\ntitle: TagInline\n---\n\nMentioning #zeta and #ops/runbook.\n[[Alpha]]\n',
);

const isMac = platform() === 'darwin';

// ---- 1. start graph-api ---------------------------------------------------

const server = spawn(
  BIN,
  [
    '--vault-root', VAULT,
    '--port', String(PORT),
    '--no-browser',
    '--assets-dir', ASSETS_DIR,
  ],
  {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, GRAPH_API_NO_BROWSER: '1' },
  },
);

const serverLog = [];
server.stdout.on('data', (b) => { serverLog.push(`out: ${b}`); });
server.stderr.on('data', (b) => { serverLog.push(`err: ${b}`); });

await new Promise((res, rej) => {
  const to = setTimeout(
    () => rej(new Error(`graph-api startup timeout\n${serverLog.join('')}`)),
    20_000,
  );
  const onData = (b) => {
    const s = b.toString();
    if (s.includes('listening') || s.includes('http://127.0.0.1')) {
      clearTimeout(to);
      res();
    }
  };
  server.stdout.on('data', onData);
  server.stderr.on('data', onData);
  server.on('exit', (code) => {
    clearTimeout(to);
    rej(new Error(`graph-api exited early (${code})\n${serverLog.join('')}`));
  });
});

// ---- 2. launch chromium with webgpu --------------------------------------

const chromiumArgs = [
  '--enable-unsafe-webgpu',
  '--enable-features=Vulkan',
  '--no-sandbox',
];
if (!isMac) {
  chromiumArgs.push('--use-angle=vulkan', '--use-gl=angle');
}

let browser;
let result;
try {
  browser = await chromium.launch({ headless: true, args: chromiumArgs });
  const ctx  = await browser.newContext({ viewport: { width: 1200, height: 800 } });
  const page = await ctx.newPage();

  const consoleLines = [];
  const pageErrors   = [];
  page.on('console',   (msg) => consoleLines.push(`${msg.type()}: ${msg.text()}`));
  page.on('pageerror', (err) => pageErrors.push(err.message));

  // Probe webgpu availability ASAP.
  await page.addInitScript(() => {
    // eslint-disable-next-line no-console
    console.log('[probe] navigator.gpu present:', !!navigator.gpu);
  });

  await page.goto(URL, { waitUntil: 'load', timeout: 30_000 });

  // Wait up to 15s for the eframe canvas to mount. If it doesn't show, we
  // continue anyway and let the pixel + error checks decide pass/fail.
  const readyDeadline = Date.now() + 15_000;
  let ready = false;
  while (Date.now() < readyDeadline) {
    const hasCanvas = await page
      .evaluate(() => !!document.getElementById('graph-canvas'))
      .catch(() => false);
    if (hasCanvas) {
      ready = true;
      break;
    }
    await page.waitForTimeout(250);
  }

  // Let the sim run a bit before the baseline screenshot.
  await page.waitForTimeout(5_000);

  await page.screenshot({ path: `${OUT}/screenshot.png`, fullPage: false });

  const canvasInfo = await page.evaluate(() => {
    const c = document.getElementById('graph-canvas');
    if (!c) return { error: 'no canvas' };
    const r = c.getBoundingClientRect();
    return {
      width: c.width,
      height: c.height,
      gpu: !!navigator.gpu,
      // Page-space center for the click step below.
      cx: r.left + r.width / 2,
      cy: r.top + r.height / 2,
    };
  });

  const brightFracOf = (png) => {
    const stride = 16;
    let bright = 0;
    let sampled = 0;
    for (let i = 0; i < png.data.length; i += stride * 4) {
      const r = png.data[i], g = png.data[i + 1], b = png.data[i + 2];
      if (r + g + b > 60) bright++;
      sampled++;
    }
    return sampled === 0 ? 0 : bright / sampled;
  };

  const baselinePng = PNG.sync.read(await readFile(`${OUT}/screenshot.png`));
  const brightFrac  = brightFracOf(baselinePng);

  // ---- check #3: click does not blank the canvas ------------------------
  //
  // Regression for the cursor-force-on-LMB bug (commit 595d8641 disabled
  // the attract force entirely). If a future change re-introduces a
  // force-on-click without bounding it, this assertion catches the
  // "screen turns black, comes back after a sec" pattern: the post-click
  // brightness must stay within an order of magnitude of the baseline,
  // *not* drop to near-zero.
  let clickBrightFrac = brightFrac;
  let clickedAt = null;
  if (canvasInfo && canvasInfo.cx && canvasInfo.cy) {
    await page.mouse.move(canvasInfo.cx, canvasInfo.cy);
    await page.mouse.down();
    // Hold for a quarter-second (a realistic click duration).
    await page.waitForTimeout(250);
    await page.mouse.up();
    clickedAt = { x: canvasInfo.cx, y: canvasInfo.cy };
    // Give the sim time to recover from any perturbation OR for the
    // selection/focus path to settle.
    await page.waitForTimeout(2_000);
    await page.screenshot({ path: `${OUT}/screenshot-post-click.png`, fullPage: false });
    const post = PNG.sync.read(await readFile(`${OUT}/screenshot-post-click.png`));
    clickBrightFrac = brightFracOf(post);
  }
  const clickDidNotBlank = clickBrightFrac > Math.max(0.005, brightFrac * 0.25);

  // ---- check #4: tag-shape fixtures round-trip through /node/:id --------
  //
  // Regression for vault-links accepting only the YAML array shape
  // (commit b533f1ab). Each fixture file uses a different YAML form;
  // every one must surface as a non-empty `tags` field on the API.
  const tagFixtureChecks = await page.evaluate(async (port) => {
    const expectations = {
      // `:id` is the basename minus `.md` per the vault loader's convention.
      TagArray:  ['alpha', 'beta'],
      TagCsv:    ['gamma', 'delta'],
      TagScalar: ['epsilon'],
      TagInline: ['zeta', 'ops/runbook'],
    };
    const results = [];
    for (const [id, expected] of Object.entries(expectations)) {
      try {
        const resp = await fetch(`http://127.0.0.1:${port}/node/${id}`);
        if (!resp.ok) {
          results.push({ id, ok: false, reason: `HTTP ${resp.status}` });
          continue;
        }
        const bytes = new Uint8Array(await resp.arrayBuffer());
        // We don't decode the protobuf in JS; we just check that every
        // expected tag string occurs verbatim in the raw bytes. Tags are
        // length-prefixed UTF-8 in the wire format, so a substring match
        // is sound: a stray collision would need the tag to appear as a
        // literal byte run somewhere else in the message, which won't
        // happen for `epsilon` or `ops/runbook`.
        const blob = new TextDecoder('utf-8', { fatal: false }).decode(bytes);
        const missing = expected.filter((t) => !blob.includes(t));
        results.push({
          id,
          ok: missing.length === 0,
          bytes: bytes.length,
          missing,
        });
      } catch (e) {
        results.push({ id, ok: false, reason: String(e) });
      }
    }
    return results;
  }, PORT);

  const tagsOk = tagFixtureChecks.every((r) => r.ok);

  // ---- check #5: compute broker health endpoint -------------------------
  //
  // Regression for the renderer having zero visibility into the back
  // half of the chain (renderer → graph-api → graph-compute). The
  // route should return JSON with at least `connected` (bool) and
  // `url` (string).
  const computeHealth = await page.evaluate(async (port) => {
    try {
      const resp = await fetch(`http://127.0.0.1:${port}/compute/health`);
      if (!resp.ok) return { ok: false, reason: `HTTP ${resp.status}` };
      const body = await resp.json();
      const shapeOk =
        typeof body.connected === 'boolean' && typeof body.url === 'string';
      return { ok: shapeOk, body, shapeOk };
    } catch (e) {
      return { ok: false, reason: String(e) };
    }
  }, PORT);

  const consoleErrors = consoleLines.filter((l) => l.startsWith('error:'));
  result = {
    ok:
      ready &&
      brightFrac > 0.01 &&
      clickDidNotBlank &&
      tagsOk &&
      computeHealth.ok &&
      pageErrors.length === 0 &&
      consoleErrors.length === 0,
    ready,
    pageErrors,
    consoleErrors: consoleErrors.slice(0, 20),
    consoleLines: consoleLines.slice(0, 50),
    canvasInfo,
    brightFrac: Number(brightFrac.toFixed(4)),
    clickBrightFrac: Number(clickBrightFrac.toFixed(4)),
    clickDidNotBlank,
    clickedAt,
    tagFixtureChecks,
    tagsOk,
    computeHealth,
    screenshot: `${OUT}/screenshot.png`,
    screenshotPostClick: `${OUT}/screenshot-post-click.png`,
  };
} catch (e) {
  result = {
    ok: false,
    error: String(e && e.stack ? e.stack : e),
    serverLog: serverLog.slice(-20).join(''),
  };
} finally {
  try { if (browser) await browser.close(); } catch {}
  try { server.kill('SIGTERM'); } catch {}
}

console.log(JSON.stringify(result, null, 2));
process.exit(result.ok ? 0 : 1);
