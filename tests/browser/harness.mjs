// Shared boot / teardown scaffolding for the headless browser tests.
//
// Both `run.mjs` (smoke) and `regression.mjs` (regression suite) need
// the same basic flow: synth vault → spawn graph-api → wait for the
// "listening" log → launch headless Chromium with WebGPU enabled →
// wait for the canvas to mount. This module exposes the two
// non-trivial bits as standalone helpers so the regression file can
// stay readable and `run.mjs` is left untouched.
//
// Intentionally tiny — anything more elaborate (probes, screenshots,
// asserts) lives in the caller. No DOM logic here.

import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';
import { platform } from 'node:os';
import { mkdirSync, writeFileSync, existsSync } from 'node:fs';

const REPO_ROOT  = resolve(process.cwd(), '..', '..');
const ASSETS_DIR = resolve(REPO_ROOT, 'crates/graph-renderer/assets/dist');
const BIN        = resolve(REPO_ROOT, 'target/release/graph-api');
const isMac      = platform() === 'darwin';

/** Ensure a tiny three-note vault exists at `path`. Idempotent. */
export function ensureSmokeVault(path = '/tmp/test-vault') {
  mkdirSync(path, { recursive: true });
  if (!existsSync(`${path}/Alpha.md`)) {
    writeFileSync(`${path}/Alpha.md`, 'See [[Beta]] and [[Gamma]].\n');
    writeFileSync(`${path}/Beta.md`,  '[[Alpha]]\n');
    writeFileSync(`${path}/Gamma.md`, '[[Alpha]] [[Beta]]\n');
  }
  return path;
}

/**
 * Spawn the graph-api binary against `vaultRoot` on `port`, resolve
 * once it logs that it's listening (or reject after 20s). Returns the
 * child process + its captured log buffer.
 */
export async function startGraphApi({ vaultRoot, port }) {
  const serverLog = [];
  const server = spawn(
    BIN,
    [
      '--vault-root', vaultRoot,
      '--port', String(port),
      '--no-browser',
      '--assets-dir', ASSETS_DIR,
    ],
    {
      stdio: ['ignore', 'pipe', 'pipe'],
      // RUST_LOG=info so the camera-input one-shot lines emit. The
      // regression suite greps for them.
      env: { ...process.env, GRAPH_API_NO_BROWSER: '1', RUST_LOG: process.env.RUST_LOG || 'info' },
    },
  );
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

  return { server, serverLog };
}

/** Standard Chromium WebGPU launch args. */
export function chromiumWebgpuArgs() {
  const args = [
    '--enable-unsafe-webgpu',
    '--enable-features=Vulkan',
    '--no-sandbox',
  ];
  if (!isMac) {
    args.push('--use-angle=vulkan', '--use-gl=angle');
  }
  return args;
}

/** Boot a Chromium page at the URL and poll until the eframe canvas
 *  mounts (#graph-canvas). Returns { browser, ctx, page, ready,
 *  consoleLines, pageErrors }. */
export async function openCanvasPage({
  url,
  viewport = { width: 1200, height: 800 },
  readyTimeoutMs = 15_000,
}) {
  const browser = await chromium.launch({ headless: true, args: chromiumWebgpuArgs() });
  const ctx     = await browser.newContext({ viewport });
  const page    = await ctx.newPage();

  const consoleLines = [];
  const pageErrors   = [];
  page.on('console',   (msg) => consoleLines.push(`${msg.type()}: ${msg.text()}`));
  page.on('pageerror', (err) => pageErrors.push(err.message));

  await page.goto(url, { waitUntil: 'load', timeout: 30_000 });

  const deadline = Date.now() + readyTimeoutMs;
  let ready = false;
  while (Date.now() < deadline) {
    const has = await page
      .evaluate(() => !!document.getElementById('graph-canvas'))
      .catch(() => false);
    if (has) { ready = true; break; }
    await page.waitForTimeout(250);
  }

  return { browser, ctx, page, ready, consoleLines, pageErrors };
}
