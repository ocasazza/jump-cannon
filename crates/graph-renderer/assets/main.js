// graph-renderer / main.js — thin shim. Loads the Rust+wgpu WASM module
// and feeds it data from graph-api. All rendering, camera math, and
// instance buffers live in the WASM. JS keeps DOM wiring (sidebar,
// search, modal) and forwards keyboard/mouse events to camera ops.
//
// Wire format split:
//   - bulk numeric (positions, edges, metrics) → raw Float32Array / Uint32Array
//   - structured (init, node metadata, search) → protobuf via protobufjs
//   - id list (/graph/ids) → JSON, fetched once at startup

import init, { WebRenderer } from '/assets/pkg/graph_renderer.js';
import protobuf from 'https://esm.sh/protobufjs@7';

// ---------- DOM handles ----------
const $stats     = document.getElementById('stats');
const $search    = document.getElementById('search');
const $include   = document.getElementById('include');
const $exclude   = document.getElementById('exclude');
const $regexBtn  = document.getElementById('regex-toggle');
const $sizeBy    = document.getElementById('size-by');
const $sizeMul   = document.getElementById('size-mul');
const $sizeMulV  = document.getElementById('size-mul-val');
const $colorBy   = document.getElementById('color-by');
const $modal     = document.getElementById('modal');
const $canvas    = document.getElementById('cosmos');
const $container = document.getElementById('container');
const $simBtns   = document.querySelectorAll('.sim-preset');
const $camFit    = document.getElementById('cam-fit');
const $camReset  = document.getElementById('cam-reset');
const $cheat     = document.getElementById('cheatsheet');

// ---------- Protobuf schema ----------
let Init = null, NodeMeta = null, SearchResults = null;

async function loadProto() {
  const root = await protobuf.load('/assets/proto/graph.proto');
  Init          = root.lookupType('jumpcannon.graph.Init');
  NodeMeta      = root.lookupType('jumpcannon.graph.NodeMeta');
  SearchResults = root.lookupType('jumpcannon.graph.SearchResults');
}

// ---------- Fetch helpers ----------
async function fetchProto(url, type) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return type.decode(new Uint8Array(await r.arrayBuffer()));
}
async function fetchF32(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return new Float32Array(await r.arrayBuffer());
}
async function fetchU32(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return new Uint32Array(await r.arrayBuffer());
}
async function fetchJson(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return r.json();
}

function setStats(text) { $stats.textContent = text; }

// ---------- Color helpers ----------
function gradient01(t) {
  t = Math.max(0, Math.min(1, t));
  return [
    0.2  + t * (0.95 - 0.2),
    0.4  + t * (0.2  - 0.4),
    0.95 + t * (0.25 - 0.95),
  ];
}
function paletteColor01(palette, idx) {
  const len = Math.max(palette.length, 1);
  const c = palette[((idx | 0) % len + len) % len];
  return c ? [c[0], c[1], c[2]] : [0.7, 0.7, 0.7];
}
function metricBounds(arr) {
  let min = Infinity, max = -Infinity;
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i];
    if (v < min) min = v;
    if (v > max) max = v;
  }
  if (!isFinite(min)) { min = 0; max = 0; }
  return { min, max };
}

// ---------- Bootstrap ----------
async function loadBootstrap() {
  setStats('loading schema…');
  await loadProto();

  setStats('loading graph…');
  const init      = await fetchProto('/graph/init', Init);
  const ids       = await fetchJson('/graph/ids');
  const positions = await fetchF32('/graph/positions');
  const edges     = await fetchU32('/graph/edges');

  setStats('loading metrics…');
  const [degree, indegree, outdegree, pagerank, betweenness, kcore, community, wcc] = await Promise.all([
    fetchF32('/graph/metrics/degree'),
    fetchF32('/graph/metrics/indegree'),
    fetchF32('/graph/metrics/outdegree'),
    fetchF32('/graph/metrics/pagerank'),
    fetchF32('/graph/metrics/betweenness'),
    fetchF32('/graph/metrics/kcore'),
    fetchF32('/graph/metrics/community'),
    fetchF32('/graph/metrics/wcc'),
  ]);
  const metrics = { degree, indegree, outdegree, pagerank, betweenness, kcore, community, wcc };
  const bounds = {};
  for (const k in metrics) bounds[k] = metricBounds(metrics[k]);

  const palette = [];
  for (let i = 0; i + 2 < init.palette.length; i += 3) {
    palette.push([init.palette[i], init.palette[i + 1], init.palette[i + 2]]);
  }

  const nNodes = Number(init.nNodes);
  const idToIdx = new Map();
  for (let i = 0; i < nNodes; i++) idToIdx.set(ids[i], i);

  // Server gives 2D; promote to 3D with random Z spread.
  const positions3 = new Float32Array(nNodes * 3);
  for (let i = 0; i < nNodes; i++) {
    positions3[i * 3 + 0] = positions[i * 2 + 0];
    positions3[i * 3 + 1] = positions[i * 2 + 1];
    positions3[i * 3 + 2] = (Math.random() - 0.5) * 200;
  }

  return { init, ids, idToIdx, edges, positions: positions3, palette, metrics, bounds, nNodes };
}

// ---------- State ----------
const state = {
  sizeBy: 'degree',
  sizeMul: 1.0,
  colorBy: 'community',
  preset: 'balanced',
};

let renderer;          // WebRenderer
let hoveredIdx = null;
let selectedIds = null; // Set<string> | null

// ---------- Accessors → Float32Array ----------
function computeSizes(boot) {
  const arr = boot.metrics[state.sizeBy];
  const { min, max } = boot.bounds[state.sizeBy] || { min: 0, max: 0 };
  const span = max - min;
  const mul = state.sizeMul;
  const out = new Float32Array(boot.nNodes);
  for (let i = 0; i < boot.nNodes; i++) {
    if (!arr || span <= 0) { out[i] = 2 * mul; continue; }
    const t = Math.sqrt((arr[i] - min) / span);
    out[i] = (1 + t * 9) * mul;
  }
  return out;
}

function computeColors(boot) {
  const dim = (selectedIds !== null);
  const out = new Float32Array(boot.nNodes * 4);
  for (let i = 0; i < boot.nNodes; i++) {
    let c;
    const key = state.colorBy;
    if (key === 'community' || key === 'wcc') {
      c = paletteColor01(boot.palette, boot.metrics[key][i] | 0);
    } else if (key === 'folder') {
      c = paletteColor01(boot.palette, boot.metrics.community[i] | 0);
    } else {
      const arr = boot.metrics[key];
      const { min, max } = boot.bounds[key];
      const span = max - min;
      c = (span <= 0) ? gradient01(0) : gradient01((arr[i] - min) / span);
    }
    let mul = 1.0;
    if (dim) mul = selectedIds.has(boot.ids[i]) ? 1.0 : 0.18;
    out[i * 4 + 0] = c[0] * mul;
    out[i * 4 + 1] = c[1] * mul;
    out[i * 4 + 2] = c[2] * mul;
    out[i * 4 + 3] = 1.0;
  }
  return out;
}

function refreshSizes(boot)  { renderer.update_sizes(computeSizes(boot)); }
function refreshColors(boot) { renderer.update_colors(computeColors(boot)); }

// ---------- Camera bounds (for fit) ----------
function computeBounds(boot) {
  let minX = Infinity, minY = Infinity, minZ = Infinity;
  let maxX = -Infinity, maxY = -Infinity, maxZ = -Infinity;
  for (let i = 0; i < boot.nNodes; i++) {
    const x = boot.positions[i * 3 + 0];
    const y = boot.positions[i * 3 + 1];
    const z = boot.positions[i * 3 + 2];
    if (x < minX) minX = x; if (x > maxX) maxX = x;
    if (y < minY) minY = y; if (y > maxY) maxY = y;
    if (z < minZ) minZ = z; if (z > maxZ) maxZ = z;
  }
  return { min: [minX, minY, minZ], max: [maxX, maxY, maxZ] };
}

// ---------- Sizing canvas to its CSS box ----------
function syncCanvasSize() {
  const r = $canvas.getBoundingClientRect();
  const dpr = Math.min(window.devicePixelRatio || 1, 1.5);
  const w = Math.max(1, Math.floor(r.width * dpr));
  const h = Math.max(1, Math.floor(r.height * dpr));
  if ($canvas.width !== w || $canvas.height !== h) {
    $canvas.width = w;
    $canvas.height = h;
    if (renderer) renderer.resize(w, h);
  }
}

// ---------- Keyboard / mouse → camera ops ----------
const keys = {};
function wireInput(boot) {
  window.addEventListener('keydown', (e) => {
    const tag = e.target && e.target.tagName;
    const inField = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
    if ((e.metaKey || e.ctrlKey) && (e.key === 'b' || e.key === 'B')) {
      e.preventDefault();
      $container.classList.toggle('sidebar-collapsed');
      requestAnimationFrame(syncCanvasSize);
      return;
    }
    if (inField) return;
    if (e.key === '?') { e.preventDefault(); $cheat.classList.toggle('hidden'); return; }
    if (e.key === ' ') { e.preventDefault(); fitAll(boot); return; }
    if (e.key === 'Escape') {
      showModal(null);
      selectedIds = null;
      refreshColors(boot);
      return;
    }
    keys[e.key.toLowerCase()] = true;
  });
  window.addEventListener('keyup', (e) => { keys[e.key.toLowerCase()] = false; });

  let dragging = false, lastX = 0, lastY = 0;
  $canvas.addEventListener('pointerdown', (e) => {
    dragging = true; lastX = e.clientX; lastY = e.clientY;
    $canvas.setPointerCapture(e.pointerId);
  });
  $canvas.addEventListener('pointerup', (e) => {
    dragging = false;
    try { $canvas.releasePointerCapture(e.pointerId); } catch (_) {}
  });
  $canvas.addEventListener('pointermove', (e) => {
    const r = $canvas.getBoundingClientRect();
    if (dragging) {
      const dx = e.clientX - lastX;
      const dy = e.clientY - lastY;
      lastX = e.clientX; lastY = e.clientY;
      renderer.cam_rotate(dx * 0.005, -dy * 0.005);
    } else {
      // Hover raycast
      const ndcX = ((e.clientX - r.left) / r.width) * 2 - 1;
      const ndcY = -(((e.clientY - r.top) / r.height) * 2 - 1);
      const idx = renderer.raycast(ndcX, ndcY);
      hoveredIdx = (idx === undefined || idx === null) ? null : idx;
      $canvas.style.cursor = hoveredIdx === null ? 'default' : 'pointer';
    }
  });
  $canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    const factor = e.deltaY > 0 ? -50 : 50;
    renderer.cam_zoom(factor);
  }, { passive: false });

  $canvas.addEventListener('click', async (e) => {
    if (hoveredIdx === null) { showModal(null); return; }
    const id = boot.ids[hoveredIdx];
    try {
      const meta = await fetchProto(`/node/${encodeURIComponent(id)}`, NodeMeta);
      showModal(meta);
    } catch (err) { console.error(err); }
  });
}

function applyKeyboardCamera(dt) {
  const speed = (keys['shift'] ? 5 : 1) * 400 * dt;
  let dx = 0, dy = 0, dz = 0;
  if (keys['w']) dz += speed;
  if (keys['s']) dz -= speed;
  if (keys['d']) dx += speed;
  if (keys['a']) dx -= speed;
  if (keys['q']) dy += speed;
  if (keys['e']) dy -= speed;
  if (keys['r']) dz += speed * 2;
  if (keys['f']) dz -= speed * 2;
  if (dx || dy || dz) renderer.cam_pan(dx, dy, dz);
}

function fitAll(boot) {
  // The Rust camera fit_to_bounds is exposed via cam_fit which uses a
  // default box; for a real fit, feed bounds in via cam_pan/zoom math here.
  // Simplest: call cam_fit and let it pick a default; Wave 2 can plumb
  // real bounds through.
  const b = computeBounds(boot);
  // Distance from sphere radius based on current FOV (60deg). Mirror the
  // Rust-side fit math so we don't need extra WASM bindings.
  const cx = (b.min[0] + b.max[0]) * 0.5;
  const cy = (b.min[1] + b.max[1]) * 0.5;
  const cz = (b.min[2] + b.max[2]) * 0.5;
  const dx = (b.max[0] - b.min[0]) * 0.5;
  const dy = (b.max[1] - b.min[1]) * 0.5;
  const dz = (b.max[2] - b.min[2]) * 0.5;
  const radius = Math.max(1, Math.sqrt(dx*dx + dy*dy + dz*dz));
  const dist = radius * 1.4 / Math.sin((60 * Math.PI / 180) * 0.5);
  // For now reuse cam_fit; the renderer's WebRenderer doesn't yet expose
  // a "fit to arbitrary bounds" — this is a known Wave 2 follow-up.
  renderer.cam_fit();
  void [cx, cy, cz, dist];
}

function resetCamera() { renderer.cam_reset(); }

// ---------- Sidebar wiring ----------
const SIM_PRESETS = { fast: {}, balanced: {}, pretty: {} };

function wireSidebar(boot) {
  $sizeBy.addEventListener('change',  () => { state.sizeBy = $sizeBy.value; refreshSizes(boot); });
  $sizeMul.addEventListener('input',  () => {
    state.sizeMul = parseFloat($sizeMul.value);
    $sizeMulV.textContent = state.sizeMul.toFixed(2);
    refreshSizes(boot);
  });
  $colorBy.addEventListener('change', () => { state.colorBy = $colorBy.value; refreshColors(boot); });

  $simBtns.forEach((btn) => {
    btn.addEventListener('click', () => {
      const preset = btn.dataset.preset;
      if (!SIM_PRESETS[preset]) return;
      state.preset = preset;
      $simBtns.forEach((b) => b.classList.toggle('active', b === btn));
      // TODO Wave 2: wire to graph-layouts WASM compute (preset → sim params).
    });
  });

  $camFit  .addEventListener('click', () => fitAll(boot));
  $camReset.addEventListener('click', () => resetCamera());
}

// ---------- Search ----------
function wireSearch(boot) {
  let regexMode = false;
  const lastSets = { search: null, include: null, exclude: null };

  const debounce = (fn, ms) => {
    let t = null;
    return (...args) => { clearTimeout(t); t = setTimeout(() => fn(...args), ms); };
  };

  function regexScan(pattern) {
    const set = new Set();
    let re;
    try { re = new RegExp(pattern); } catch (_) { return set; }
    for (const id of boot.ids) if (re.test(id)) set.add(id);
    return set;
  }
  async function serverSearch(q) {
    const r = await fetchProto(`/search?q=${encodeURIComponent(q)}`, SearchResults);
    return new Set(r.ids || []);
  }
  function applyComposite() {
    const { search, include, exclude } = lastSets;
    if (search === null && include === null && exclude === null) {
      selectedIds = null;
      refreshColors(boot);
      return;
    }
    let final;
    if (search !== null && include !== null) {
      final = new Set();
      for (const id of search) if (include.has(id)) final.add(id);
    } else if (search !== null) { final = new Set(search); }
    else if (include !== null) { final = new Set(include); }
    else { final = new Set(boot.ids); }
    if (exclude !== null) for (const id of exclude) final.delete(id);
    selectedIds = final;
    refreshColors(boot);
  }
  function makeRowHandler(slot, inputEl, allowRegex) {
    return debounce(async () => {
      const q = inputEl.value.trim();
      if (!q) { lastSets[slot] = null; applyComposite(); return; }
      try {
        if (allowRegex && regexMode) lastSets[slot] = regexScan(q);
        else                          lastSets[slot] = await serverSearch(q);
        applyComposite();
      } catch (e) { console.error(e); }
    }, 150);
  }

  $search .addEventListener('input', makeRowHandler('search',  $search,  true));
  $include.addEventListener('input', makeRowHandler('include', $include, false));
  $exclude.addEventListener('input', makeRowHandler('exclude', $exclude, false));
  $regexBtn.addEventListener('click', () => {
    regexMode = !regexMode;
    $regexBtn.classList.toggle('active', regexMode);
    $search.dispatchEvent(new Event('input'));
  });
}

// ---------- Modal ----------
function showModal(meta) {
  if (!meta) { $modal.classList.add('hidden'); return; }
  const tags = (meta.tags || []).map(t => `#${t}`).join(' ');
  $modal.innerHTML = `
    <h2>${meta.title || meta.path || '?'}</h2>
    <div class="kv">
      <span>path</span><span>${meta.path || ''}</span>
      <span>folder</span><span>${meta.folder || ''}</span>
      <span>doctype</span><span>${meta.doctype || ''}</span>
      <span>tags</span><span>${tags}</span>
      <span>degree</span><span>${meta.degree ?? ''}</span>
      <span>indegree</span><span>${meta.indegree ?? ''}</span>
      <span>outdegree</span><span>${meta.outdegree ?? ''}</span>
      <span>pagerank</span><span>${(meta.pagerank ?? 0).toFixed(4)}</span>
      <span>betweenness</span><span>${(meta.betweenness ?? 0).toFixed(4)}</span>
      <span>kcore</span><span>${meta.kcore ?? ''}</span>
      <span>community</span><span>${meta.community ?? ''}</span>
      <span>wcc</span><span>${meta.wcc ?? ''}</span>
    </div>
  `;
  $modal.classList.remove('hidden');
}

// ---------- main ----------
async function main() {
  await init('/assets/pkg/graph_renderer_bg.wasm');

  const boot = await loadBootstrap();

  syncCanvasSize();
  renderer = new WebRenderer();

  const colors = computeColors(boot);
  const sizes  = computeSizes(boot);
  await renderer.init('cosmos', boot.positions, boot.edges, colors, sizes);
  renderer.resize($canvas.width, $canvas.height);

  setStats(
    `${boot.nNodes.toLocaleString()} nodes • ` +
    `${Number(boot.init.nEdges).toLocaleString()} edges • ` +
    `${boot.init.numCommunities} communities`
  );

  wireSidebar(boot);
  wireSearch(boot);
  wireInput(boot);

  const ro = new ResizeObserver(syncCanvasSize);
  ro.observe($canvas);
  window.addEventListener('resize', syncCanvasSize);

  let lastT = performance.now();
  function frame() {
    const now = performance.now();
    const dt = Math.min(0.1, (now - lastT) / 1000);
    lastT = now;
    applyKeyboardCamera(dt);
    try { renderer.render(); } catch (e) { console.error(e); }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

main().catch((e) => {
  console.error(e);
  setStats(`error: ${e.message || e}`);
});
