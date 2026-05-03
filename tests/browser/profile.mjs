// Headless browser PROFILER for the graph-renderer.
//
// Same setup as run.mjs (graph-api + headless Chromium + WebGPU), but
// instead of a single screenshot + black-canvas check, this script:
//
//   1. Boots the page and waits for the canvas + initial settle.
//   2. Installs a frame timer (rAF deltas) into `window` and lets it run.
//   3. Optionally toggles features (palette open, DoF on) so we can compare
//      hot paths.
//   4. Reports per-phase: avg FPS, p50/p95/p99 frame time, jank count
//      (>33ms frames), longest frame, and a few app-side counters
//      (n_nodes, n_edges, canvas size).
//
// Output: a single JSON line on stdout summarising each measured phase
// + tests/browser/out/profile-*.png screenshots per phase.
// Exit 0 always — this is a diagnostic, not a gate.
//
// Tunables (env):
//   PROFILE_PHASE_MS    duration of each measurement phase (default 4000)
//   PROFILE_PORT        graph-api port (default 47896)
//   VAULT_ROOT          vault dir (default /tmp/test-vault)

import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { platform } from 'node:os';

const REPO_ROOT  = resolve(process.cwd(), '..', '..');
const PORT       = Number(process.env.PROFILE_PORT || 47896);
const URL        = `http://127.0.0.1:${PORT}/`;
const OUT        = resolve('out');
const ASSETS_DIR = resolve(REPO_ROOT, 'crates/graph-renderer/assets/dist');
const BIN        = resolve(REPO_ROOT, 'target/release/graph-api');
const PHASE_MS   = Number(process.env.PROFILE_PHASE_MS || 4000);
const SYNTH_N    = Number(process.env.PROFILE_VAULT_NODES || 0); // 0 = use VAULT_ROOT
const SYNTH_DEG  = Number(process.env.PROFILE_VAULT_DEGREE || 6);
const VAULT      = SYNTH_N > 0
  ? `/tmp/synth-vault-${SYNTH_N}`
  : (process.env.VAULT_ROOT || '/tmp/test-vault');

mkdirSync(OUT, { recursive: true });

// ---- 0. (optional) synthesize a load-test vault --------------------------
if (SYNTH_N > 0) {
  if (!existsSync(VAULT) || !existsSync(`${VAULT}/N0.md`)) {
    mkdirSync(VAULT, { recursive: true });
    // Deterministic PRNG so re-runs hit the same graph.
    let seed = 0x9e3779b1;
    const rand = () => {
      seed = (seed * 1664525 + 1013904223) >>> 0;
      return seed / 0x1_0000_0000;
    };
    const { writeFileSync } = await import('node:fs');
    process.stderr.write(`→ synthesising ${SYNTH_N}-node vault at ${VAULT}\n`);
    for (let i = 0; i < SYNTH_N; i++) {
      const links = new Set();
      const k = SYNTH_DEG;
      while (links.size < k) {
        const j = Math.floor(rand() * SYNTH_N);
        if (j !== i) links.add(j);
      }
      const body = Array.from(links).map((j) => `[[N${j}]]`).join(' ') + '\n';
      writeFileSync(`${VAULT}/N${i}.md`, body);
    }
  }
}

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
  { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, GRAPH_API_NO_BROWSER: '1' } },
);

const serverLog = [];
server.stdout.on('data', (b) => serverLog.push(`out: ${b}`));
server.stderr.on('data', (b) => serverLog.push(`err: ${b}`));

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
if (!isMac) chromiumArgs.push('--use-angle=vulkan', '--use-gl=angle');

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

  await page.goto(URL, { waitUntil: 'load', timeout: 30_000 });

  // Wait for the canvas + a 3 s warm-up so the force sim isn't dominating
  // the first measurement phase.
  const readyDeadline = Date.now() + 15_000;
  let ready = false;
  while (Date.now() < readyDeadline) {
    const hasCanvas = await page
      .evaluate(() => !!document.getElementById('graph-canvas'))
      .catch(() => false);
    if (hasCanvas) { ready = true; break; }
    await page.waitForTimeout(250);
  }
  if (!ready) throw new Error('canvas never mounted');
  await page.waitForTimeout(3_000);

  // Install a rAF-based frame timer once. Each call to startFrameTimer
  // resets the buffer; stopFrameTimer returns the captured deltas.
  await page.evaluate(() => {
    window.__frameTimer = {
      running: false,
      deltas: [],
      lastT: 0,
      tick(t) {
        if (!window.__frameTimer.running) return;
        if (window.__frameTimer.lastT) {
          window.__frameTimer.deltas.push(t - window.__frameTimer.lastT);
        }
        window.__frameTimer.lastT = t;
        requestAnimationFrame(window.__frameTimer.tick);
      },
    };
    window.startFrameTimer = () => {
      window.__frameTimer.running = true;
      window.__frameTimer.deltas = [];
      window.__frameTimer.lastT = 0;
      requestAnimationFrame(window.__frameTimer.tick);
    };
    window.stopFrameTimer = () => {
      window.__frameTimer.running = false;
      return window.__frameTimer.deltas.slice();
    };
  });

  const canvasInfo = await page.evaluate(() => {
    const c = document.getElementById('graph-canvas');
    return c
      ? { width: c.width, height: c.height, gpu: !!navigator.gpu }
      : { error: 'no canvas', gpu: !!navigator.gpu };
  });

  const phases = [];
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Profiler.enable');
  await cdp.send('Profiler.setSamplingInterval', { interval: 200 }); // µs

  // Helper to run one measurement phase + capture a CPU profile.
  const measure = async (label, before, after) => {
    if (before) await before();
    await cdp.send('Profiler.start');
    await page.evaluate(() => window.startFrameTimer());
    await page.waitForTimeout(PHASE_MS);
    const deltas = await page.evaluate(() => window.stopFrameTimer());
    const { profile } = await cdp.send('Profiler.stop');
    await page.screenshot({ path: `${OUT}/profile-${label}.png` });
    if (after) await after();
    const cpuPath = `${OUT}/profile-${label}.cpuprofile`;
    writeFileSync(cpuPath, JSON.stringify(profile));
    const hot = topSelfTime(profile, 12);
    phases.push({ label, ...summarise(deltas), hot, cpuprofile: cpuPath });
  };

  // Phase 1: idle baseline (current default state — DoF off).
  await measure('idle');

  // Phase 2: open the command palette via Ctrl+P, hold for the phase.
  await measure(
    'palette-open',
    async () => {
      await page.keyboard.down('Control');
      await page.keyboard.press('KeyP');
      await page.keyboard.up('Control');
    },
    async () => {
      // Close palette before the next phase.
      await page.keyboard.press('Escape');
    },
  );

  // Phase 3: simulate a keyboard nudge of the bare F shortcut to trigger
  // the FitCamera action — verifies the F binding doesn't auto-repeat.
  await measure(
    'fit-camera',
    async () => { await page.keyboard.press('KeyF'); },
  );

  result = {
    ok: pageErrors.length === 0,
    vault: VAULT,
    synthN: SYNTH_N,
    canvasInfo,
    pageErrors,
    consoleErrors: consoleLines.filter((l) => l.startsWith('error:')).slice(0, 10),
    phases,
    phaseMs: PHASE_MS,
  };
  // Human-readable header before the JSON body.
  process.stderr.write('\n=== profile summary ===\n');
  process.stderr.write(`vault: ${VAULT} (synth N=${SYNTH_N || '-'})\n`);
  for (const p of phases) {
    process.stderr.write(
      `[${p.label.padEnd(14)}] ${p.fps} fps  mean ${p.mean_ms}ms  p95 ${p.p95_ms}ms  p99 ${p.p99_ms}ms  jank ${p.jank_pct}%\n`,
    );
    const nontrivial = p.hot.filter(
      (h) => h.fn !== '(idle)' && h.fn !== '(program)' && h.fn !== '(garbage collector)',
    );
    for (const h of nontrivial.slice(0, 8)) {
      process.stderr.write(`    ${h.self_pct.toString().padStart(5)}%  ${h.self_ms.toString().padStart(7)}ms  ${h.fn} ${h.url ? '(' + h.url + ')' : ''}\n`);
    }
  }
  process.stderr.write('\n');
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
process.exit(0);

/**
 * Sum self-time per node in a V8 .cpuprofile and return the top-N
 * by self-time. Self-time = (samples × interval). For wasm, function
 * names look like `wasm-function[12345]` unless the bundle ships
 * a name section — wasm-bindgen retains them in dev builds, so the
 * release build will show indices but the URL points at the .wasm.
 */
function topSelfTime(profile, n) {
  if (!profile || !profile.nodes || !profile.samples || !profile.timeDeltas) {
    return [];
  }
  // self time per nodeId (in µs)
  const selfUs = new Map();
  for (let i = 0; i < profile.samples.length; i++) {
    const id = profile.samples[i];
    const dt = profile.timeDeltas[i] || 0;
    selfUs.set(id, (selfUs.get(id) || 0) + dt);
  }
  const totalUs = Array.from(selfUs.values()).reduce((s, x) => s + x, 0) || 1;
  const rows = profile.nodes.map((node) => {
    const cf = node.callFrame || {};
    const self = selfUs.get(node.id) || 0;
    return {
      fn: cf.functionName || '(anonymous)',
      url: cf.url ? cf.url.replace(/^.*\//, '') : '',
      self_ms: Number((self / 1000).toFixed(2)),
      self_pct: Number((100 * self / totalUs).toFixed(1)),
    };
  });
  rows.sort((a, b) => b.self_ms - a.self_ms);
  return rows.filter((r) => r.self_ms > 0).slice(0, n);
}

function summarise(deltas) {
  if (!deltas || deltas.length === 0) {
    return { frames: 0, fps: 0, mean_ms: 0, p50_ms: 0, p95_ms: 0, p99_ms: 0, max_ms: 0, jank_frames: 0 };
  }
  const sorted = deltas.slice().sort((a, b) => a - b);
  const sum = deltas.reduce((s, x) => s + x, 0);
  const mean = sum / deltas.length;
  const pct = (p) => sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))];
  const jank = deltas.filter((d) => d > 33).length;
  return {
    frames: deltas.length,
    fps: Number((1000 / mean).toFixed(1)),
    mean_ms: Number(mean.toFixed(2)),
    p50_ms: Number(pct(50).toFixed(2)),
    p95_ms: Number(pct(95).toFixed(2)),
    p99_ms: Number(pct(99).toFixed(2)),
    max_ms: Number(Math.max(...deltas).toFixed(2)),
    jank_frames: jank,
    jank_pct: Number((100 * jank / deltas.length).toFixed(1)),
  };
}
