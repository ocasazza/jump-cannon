// Headless browser PERF GATE for the graph-renderer.
//
// Single-phase, single-purpose: boot the WASM frontend against a synthetic
// vault with a fixed node count, let it warm up + (optionally) settle,
// then measure idle FPS via rAF deltas. Fails the process if FPS / p99
// frame time / jank pct miss thresholds. Designed to be wired into CI as
// a gate against perf regressions.
//
// Tunables (env):
//   PERF_VAULT_NODES   synth vault size (default 1000)
//   PERF_VAULT_DEGREE  avg edges per node (default 4)
//   PERF_PHASE_MS      measurement duration (default 4000)
//   PERF_MIN_FPS       fail if mean FPS below this (default 50)
//   PERF_MAX_P99_MS    fail if p99 frame time above this (default 25)
//   PERF_MAX_JANK_PCT  fail if % frames > 33ms exceeds this (default 5)
//   PERF_PORT          graph-api port (default 47897)
//
// On exit, also dumps an AI-readable flame graph for the measurement
// phase so a regression can be diagnosed without Chrome DevTools.

import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync, existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { platform } from 'node:os';
import { PNG } from 'pngjs';

const REPO_ROOT  = resolve(process.cwd(), '..', '..');
const PORT       = Number(process.env.PERF_PORT || 47897);
const URL        = `http://127.0.0.1:${PORT}/`;
const OUT        = resolve('out');
const ASSETS_DIR = resolve(REPO_ROOT, 'crates/graph-renderer/assets/dist');
const BIN        = resolve(REPO_ROOT, 'target/release/graph-api');
const PHASE_MS   = Number(process.env.PERF_PHASE_MS || 4000);
const SYNTH_N    = Number(process.env.PERF_VAULT_NODES || 1000);
const SYNTH_DEG  = Number(process.env.PERF_VAULT_DEGREE || 4);
const MIN_FPS    = Number(process.env.PERF_MIN_FPS || 50);
const MAX_P99_MS = Number(process.env.PERF_MAX_P99_MS || 25);
const MAX_JANK   = Number(process.env.PERF_MAX_JANK_PCT || 5);
const VAULT      = `/tmp/synth-vault-${SYNTH_N}`;

mkdirSync(OUT, { recursive: true });

// ---- 0. synth vault (deterministic) --------------------------------------
if (!existsSync(VAULT) || !existsSync(`${VAULT}/N0.md`)) {
  mkdirSync(VAULT, { recursive: true });
  let seed = 0x9e3779b1;
  const rand = () => { seed = (seed * 1664525 + 1013904223) >>> 0; return seed / 0x1_0000_0000; };
  process.stderr.write(`→ synthesising ${SYNTH_N}-node vault at ${VAULT}\n`);
  for (let i = 0; i < SYNTH_N; i++) {
    const links = new Set();
    while (links.size < SYNTH_DEG) {
      const j = Math.floor(rand() * SYNTH_N);
      if (j !== i) links.add(j);
    }
    writeFileSync(`${VAULT}/N${i}.md`, Array.from(links).map((j) => `[[N${j}]]`).join(' ') + '\n');
  }
}

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
  const to = setTimeout(() => rej(new Error(`graph-api startup timeout\n${serverLog.join('')}`)), 30_000);
  const onData = (b) => {
    const s = b.toString();
    if (s.includes('listening') || s.includes('http://127.0.0.1')) {
      clearTimeout(to); res();
    }
  };
  server.stdout.on('data', onData);
  server.stderr.on('data', onData);
  server.on('exit', (code) => { clearTimeout(to); rej(new Error(`graph-api exited early (${code})\n${serverLog.join('')}`)); });
});

// ---- 2. launch chromium with webgpu --------------------------------------
const isMac = platform() === 'darwin';
const chromiumArgs = ['--enable-unsafe-webgpu', '--enable-features=Vulkan', '--no-sandbox'];
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

  // Wait for canvas mount + "graph loaded:" log line.
  let ready = false;
  for (let t = 0; t < 30_000; t += 250) {
    if (await page.evaluate(() => !!document.getElementById('graph-canvas')).catch(() => false)) {
      ready = true; break;
    }
    await page.waitForTimeout(250);
  }
  if (!ready) throw new Error('canvas never mounted');
  let loaded = false;
  for (let t = 0; t < 60_000; t += 250) {
    if (consoleLines.some((l) => l.includes('graph loaded:'))) { loaded = true; break; }
    await page.waitForTimeout(250);
  }
  if (!loaded) {
    process.stderr.write(`WARN: no "graph loaded:" log; continuing anyway.\n`);
  }
  // Warmup before measuring — sim is most expensive in early frames.
  await page.waitForTimeout(3_000);

  // Install rAF timer + start CPU profile.
  await page.evaluate(() => {
    window.__ft = { running: false, deltas: [], lastT: 0,
      tick(t) {
        if (!window.__ft.running) return;
        if (window.__ft.lastT) window.__ft.deltas.push(t - window.__ft.lastT);
        window.__ft.lastT = t;
        requestAnimationFrame(window.__ft.tick);
      },
    };
    window.__startFt = () => { window.__ft.running = true; window.__ft.deltas = []; window.__ft.lastT = 0; requestAnimationFrame(window.__ft.tick); };
    window.__stopFt = () => { window.__ft.running = false; return window.__ft.deltas.slice(); };
  });
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Profiler.enable');
  await cdp.send('Profiler.setSamplingInterval', { interval: 200 });
  await cdp.send('Profiler.start');
  await page.evaluate(() => window.__startFt());
  await page.waitForTimeout(PHASE_MS);
  const deltas = await page.evaluate(() => window.__stopFt());
  const { profile } = await cdp.send('Profiler.stop');

  await page.screenshot({ path: `${OUT}/perf-idle.png` });
  writeFileSync(`${OUT}/perf-idle.cpuprofile`, JSON.stringify(profile));
  const flame = flameTree(profile);
  writeFileSync(`${OUT}/perf-idle.flame.txt`, flame);

  const stats = summarise(deltas);
  const fail = [];
  if (stats.fps < MIN_FPS)        fail.push(`fps ${stats.fps} < ${MIN_FPS}`);
  if (stats.p99_ms > MAX_P99_MS)  fail.push(`p99 ${stats.p99_ms}ms > ${MAX_P99_MS}ms`);
  if (stats.jank_pct > MAX_JANK)  fail.push(`jank ${stats.jank_pct}% > ${MAX_JANK}%`);

  result = {
    ok: pageErrors.length === 0 && fail.length === 0,
    vault: VAULT,
    synthN: SYNTH_N,
    thresholds: { min_fps: MIN_FPS, max_p99_ms: MAX_P99_MS, max_jank_pct: MAX_JANK },
    pageErrors,
    consoleErrors: consoleLines.filter((l) => l.startsWith('error:')).slice(0, 10),
    stats,
    failures: fail,
    flame: `${OUT}/perf-idle.flame.txt`,
  };

  // Human-readable summary on stderr.
  process.stderr.write(`\n=== perf gate (synth N=${SYNTH_N}) ===\n`);
  process.stderr.write(`  fps     ${stats.fps}    (min ${MIN_FPS})  ${stats.fps >= MIN_FPS ? 'PASS' : 'FAIL'}\n`);
  process.stderr.write(`  p99     ${stats.p99_ms}ms (max ${MAX_P99_MS}ms)  ${stats.p99_ms <= MAX_P99_MS ? 'PASS' : 'FAIL'}\n`);
  process.stderr.write(`  jank    ${stats.jank_pct}%  (max ${MAX_JANK}%)  ${stats.jank_pct <= MAX_JANK ? 'PASS' : 'FAIL'}\n`);
  process.stderr.write(`  mean    ${stats.mean_ms}ms\n`);
  process.stderr.write(`  p50/p95 ${stats.p50_ms}/${stats.p95_ms}ms\n`);
  process.stderr.write(`  flame:  ${OUT}/perf-idle.flame.txt\n\n`);
  if (fail.length) {
    process.stderr.write(`FAIL: ${fail.join('; ')}\n`);
    process.stderr.write(`---- top of flame tree ----\n${flame.split('\n').slice(0, 20).join('\n')}\n`);
  }
} catch (e) {
  result = { ok: false, error: String(e?.stack || e), serverLog: serverLog.slice(-20).join('') };
} finally {
  try { if (browser) await browser.close(); } catch {}
  try { server.kill('SIGTERM'); } catch {}
}

console.log(JSON.stringify(result, null, 2));
process.exit(result.ok ? 0 : 1);

// ---- helpers (mirrors profile.mjs; kept here so perf.mjs is standalone) ---

function flameTree(profile) {
  if (!profile?.nodes?.length) return '(empty cpuprofile)';
  const byId = new Map();
  for (const n of profile.nodes) byId.set(n.id, n);
  const selfUs = new Map();
  for (let i = 0; i < profile.samples.length; i++) {
    const id = profile.samples[i];
    const dt = profile.timeDeltas[i] || 0;
    selfUs.set(id, (selfUs.get(id) || 0) + dt);
  }
  const totalUs = new Map();
  const visit = (id) => {
    if (totalUs.has(id)) return totalUs.get(id);
    const n = byId.get(id);
    let t = selfUs.get(id) || 0;
    if (n?.children) for (const c of n.children) t += visit(c);
    totalUs.set(id, t);
    return t;
  };
  for (const n of profile.nodes) visit(n.id);
  const root = profile.nodes.find((n) => n.callFrame?.functionName === '(root)') || profile.nodes[0];
  const totalAll = totalUs.get(root.id) || 1;
  const minPct = 0.2;
  const lines = [];
  const fmt = (id, depth, prefix, isLast) => {
    const n = byId.get(id);
    const cf = n.callFrame || {};
    const tUs = totalUs.get(id) || 0;
    const sUs = selfUs.get(id) || 0;
    const tPct = (100 * tUs) / totalAll;
    if (depth > 0 && tPct < minPct) return;
    const tMs = (tUs / 1000).toFixed(1).padStart(7);
    const sMs = (sUs / 1000).toFixed(1).padStart(7);
    const tPctStr = tPct.toFixed(1).padStart(5);
    const sPctStr = ((100 * sUs) / totalAll).toFixed(1).padStart(5);
    const url = cf.url ? cf.url.replace(/^.*\//, '') : '';
    const branch = depth === 0 ? '' : prefix + (isLast ? '└── ' : '├── ');
    lines.push(`${tMs}ms ${tPctStr}% (self ${sMs}ms ${sPctStr}%)  ${branch}${cf.functionName || '(anon)'}${url ? '  [' + url + ']' : ''}`);
    const childPrefix = depth === 0 ? '' : prefix + (isLast ? '    ' : '│   ');
    const kids = (n.children || [])
      .map((c) => [c, totalUs.get(c) || 0])
      .filter(([, t]) => (100 * t) / totalAll >= minPct)
      .sort((a, b) => b[1] - a[1]);
    for (let i = 0; i < kids.length; i++) fmt(kids[i][0], depth + 1, childPrefix, i === kids.length - 1);
  };
  fmt(root.id, 0, '', true);
  return [
    `# Flame tree — total ${(totalAll / 1000).toFixed(1)}ms across ${profile.samples.length} samples`,
    `# Columns: total_ms total_pct% (self_ms self_pct%)  call`,
    `# Pruned: nodes contributing < ${minPct}% of total time omitted.`,
    '', ...lines,
  ].join('\n');
}

function summarise(deltas) {
  if (!deltas?.length) return { frames: 0, fps: 0, mean_ms: 0, p50_ms: 0, p95_ms: 0, p99_ms: 0, max_ms: 0, jank_frames: 0, jank_pct: 0 };
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
