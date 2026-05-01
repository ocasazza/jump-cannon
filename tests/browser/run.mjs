// Headless WebGPU browser test for the graph-renderer.
//
// Spins up `graph-api` as a subprocess against a tiny test vault, opens the
// served page in headless Chromium with WebGPU enabled, lets the force sim
// run for a few seconds, screenshots the canvas, and asserts:
//   1. no page errors
//   2. the renderer's "live force sim active" log fired
//   3. the captured screenshot isn't ~all-black
//
// Output: tests/browser/out/screenshot.png + a single JSON line on stdout.
// Exit 0 = ok, 1 = something failed.

import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { platform } from 'node:os';
import { PNG } from 'pngjs';

const REPO_ROOT  = resolve(process.cwd(), '..', '..');
const VAULT      = process.env.VAULT_ROOT || '/tmp/test-vault';
const PORT       = Number(process.env.TEST_PORT || 47895);
const URL        = `http://127.0.0.1:${PORT}/`;
const OUT        = resolve('out');
const ASSETS_DIR = resolve(REPO_ROOT, 'crates/graph-renderer/assets');
const BIN        = resolve(REPO_ROOT, 'target/release/graph-api');

mkdirSync(OUT, { recursive: true });

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

  // Wait up to 15s for the renderer's init log. If it doesn't show, we
  // continue anyway and let the pixel + error checks decide pass/fail.
  const readyDeadline = Date.now() + 15_000;
  let ready = false;
  while (Date.now() < readyDeadline) {
    if (consoleLines.some((l) => l.includes('[graph-renderer] live force sim active'))) {
      ready = true;
      break;
    }
    await page.waitForTimeout(250);
  }

  // Let the sim run a bit.
  await page.waitForTimeout(5_000);

  await page.screenshot({ path: `${OUT}/screenshot.png`, fullPage: false });

  const canvasInfo = await page.evaluate(() => {
    const c = document.getElementById('cosmos');
    if (!c) return { error: 'no canvas' };
    return { width: c.width, height: c.height, gpu: !!navigator.gpu };
  });

  // Pixel check on the screenshot bytes. Sample every 16th pixel.
  const png = PNG.sync.read(await readFile(`${OUT}/screenshot.png`));
  const stride = 16;
  let bright = 0;
  let sampled = 0;
  for (let i = 0; i < png.data.length; i += stride * 4) {
    const r = png.data[i], g = png.data[i + 1], b = png.data[i + 2];
    if (r + g + b > 60) bright++;
    sampled++;
  }
  const brightFrac = sampled === 0 ? 0 : bright / sampled;

  const consoleErrors = consoleLines.filter((l) => l.startsWith('error:'));
  result = {
    ok:
      ready &&
      brightFrac > 0.01 &&
      pageErrors.length === 0 &&
      consoleErrors.length === 0,
    ready,
    pageErrors,
    consoleErrors: consoleErrors.slice(0, 20),
    consoleLines: consoleLines.slice(0, 50),
    canvasInfo,
    brightFrac: Number(brightFrac.toFixed(4)),
    screenshot: `${OUT}/screenshot.png`,
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
