// graph-renderer / main.js — thin shim. Loads the Rust+wgpu WASM module
// and feeds it data from graph-api. All rendering, force-sim compute, and
// instance buffers live in WASM. JS keeps DOM wiring (sidebar, search,
// modal, focus + cursor sliders) and forwards keyboard/mouse events.
//
// Wave 2: graph-renderer now owns a GpuForceLayout (from graph-layouts)
// against the same wgpu device + a shared positions storage buffer. The
// vertex shaders read from that same buffer — compute writes, render
// reads, no CPU copy per frame. Edges follow nodes automatically.
//
// Frame loop: `r.step()` runs N compute dispatches and one render. JS
// updates only sim params (sliders, cursor tool) via update_layout_options
// or the dedicated `set_cursor_force` / `set_focus_plane` setters.

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
const $focusZ    = document.getElementById('focus-z');
const $focusZV   = document.getElementById('focus-z-val');
const $focusT    = document.getElementById('focus-thickness');
const $focusTV   = document.getElementById('focus-thickness-val');
const $curR      = document.getElementById('cursor-radius');
const $curRV     = document.getElementById('cursor-radius-val');
const $curS      = document.getElementById('cursor-strength');
const $curSV     = document.getElementById('cursor-strength-val');
const $curD      = document.getElementById('cursor-depth');
const $curDV     = document.getElementById('cursor-depth-val');

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
  const r = await fetch(url); if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return type.decode(new Uint8Array(await r.arrayBuffer()));
}
async function fetchF32(url) {
  const r = await fetch(url); if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return new Float32Array(await r.arrayBuffer());
}
async function fetchU32(url) {
  const r = await fetch(url); if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return new Uint32Array(await r.arrayBuffer());
}
async function fetchJson(url) {
  const r = await fetch(url); if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  return r.json();
}
function setStats(text) { $stats.textContent = text; }

// ---------- Color helpers ----------
function gradient01(t) {
  t = Math.max(0, Math.min(1, t));
  return [0.2 + t*(0.95-0.2), 0.4 + t*(0.2-0.4), 0.95 + t*(0.25-0.95)];
}
function paletteColor01(palette, idx) {
  const len = Math.max(palette.length, 1);
  const c = palette[((idx | 0) % len + len) % len];
  return c ? [c[0], c[1], c[2]] : [0.7, 0.7, 0.7];
}
function metricBounds(arr) {
  let min = Infinity, max = -Infinity;
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i]; if (v < min) min = v; if (v > max) max = v;
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
    fetchF32('/graph/metrics/degree'),    fetchF32('/graph/metrics/indegree'),
    fetchF32('/graph/metrics/outdegree'), fetchF32('/graph/metrics/pagerank'),
    fetchF32('/graph/metrics/betweenness'), fetchF32('/graph/metrics/kcore'),
    fetchF32('/graph/metrics/community'), fetchF32('/graph/metrics/wcc'),
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

  // Server gives 2D; promote to 3D with random Z spread (compute will
  // scramble these soon enough).
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
  sizeBy: 'degree', sizeMul: 1.0, colorBy: 'community', preset: 'balanced',
};
let renderer;
let hoveredIdx = null;
let selectedIds = null;

// Sim params mirrored on the JS side. We seed `cursor_*` from the slider
// values but only push them via set_cursor_force when LMB/RMB are held.
const baseLayout = {
  repulsion: 200, spring_k: 0.08, spring_len: 30,
  gravity: 0.005, damping: 0.78, dt: 0.04,
  cursor_pos: [0, 0, 0], cursor_radius: 0, cursor_strength: 0,
  steps_per_call: 8,
  // Spatial-hash grid + cooling. repulsion_radius bounds per-pair work to
  // ~27 cells (3x3x3 neighbor walk). cooling_alpha < 1 cools damping per
  // call toward cooling_floor. energy_threshold > 0 short-circuits when
  // KE drops below it.
  repulsion_radius: 120,
  cooling_alpha: 0.998,
  cooling_floor: 0.5,
  energy_threshold: 0.05,
  grid_enabled: true,
};
const SIM_PRESETS = {
  fast:     { ...baseLayout, repulsion: 150, damping: 0.70, steps_per_call: 16 },
  balanced: { ...baseLayout },
  pretty:   { ...baseLayout, repulsion: 300, damping: 0.92, dt: 0.025, steps_per_call: 4 },
};

// ---------- Accessors → Float32Array ----------
function computeSizes(boot) {
  const arr = boot.metrics[state.sizeBy];
  const { min, max } = boot.bounds[state.sizeBy] || { min: 0, max: 0 };
  const span = max - min, mul = state.sizeMul;
  // Pixel-space sizes — billboarded points, so values are interpreted as
  // pixel radii by the vertex shader. Range with mul=1: ~4-8px; with mul=4:
  // ~16-32px for the largest hubs.
  const base = 4.0;
  const out = new Float32Array(boot.nNodes);
  for (let i = 0; i < boot.nNodes; i++) {
    if (!arr || span <= 0) { out[i] = base * mul; continue; }
    const t = Math.sqrt((arr[i] - min) / span);
    out[i] = (base + t * base) * mul;
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
      const arr = boot.metrics[key]; const { min, max } = boot.bounds[key];
      const span = max - min;
      c = (span <= 0) ? gradient01(0) : gradient01((arr[i] - min) / span);
    }
    let mul = 1.0;
    if (dim) mul = selectedIds.has(boot.ids[i]) ? 1.0 : 0.18;
    out[i*4+0] = c[0]*mul; out[i*4+1] = c[1]*mul; out[i*4+2] = c[2]*mul; out[i*4+3] = 1.0;
  }
  return out;
}
function refreshSizes(boot)  { renderer.update_sizes(computeSizes(boot)); }
function refreshColors(boot) { renderer.update_colors(computeColors(boot)); }

// ---------- Camera bounds (for fit) ----------
function computeBounds(boot) {
  let mnX=Infinity,mnY=Infinity,mnZ=Infinity, mxX=-Infinity,mxY=-Infinity,mxZ=-Infinity;
  for (let i = 0; i < boot.nNodes; i++) {
    const x = boot.positions[i*3+0], y = boot.positions[i*3+1], z = boot.positions[i*3+2];
    if (x<mnX)mnX=x; if (x>mxX)mxX=x; if (y<mnY)mnY=y; if (y>mxY)mxY=y; if (z<mnZ)mnZ=z; if (z>mxZ)mxZ=z;
  }
  return { min: [mnX, mnY, mnZ], max: [mxX, mxY, mxZ] };
}

// ---------- Sizing ----------
function syncCanvasSize() {
  const r = $canvas.getBoundingClientRect();
  const dpr = Math.min(window.devicePixelRatio || 1, 1.5);
  const w = Math.max(1, Math.floor(r.width * dpr));
  const h = Math.max(1, Math.floor(r.height * dpr));
  if ($canvas.width !== w || $canvas.height !== h) {
    $canvas.width = w; $canvas.height = h;
    if (renderer) renderer.resize(w, h);
  }
}

// ---------- 6DoF cursor force tool ----------
const cursor = {
  ndcX: 0, ndcY: 0,        // screen position in NDC ([-1,1] each)
  depth: 800,              // distance forward from camera
  radius: 120,
  strength: 200,
  lmb: false, rmb: false,
  enabled: false,
};
function pushCursorForce() {
  if (!renderer) return;
  if (!cursor.enabled || (!cursor.lmb && !cursor.rmb)) {
    renderer.set_cursor_force(0, 0, 0, 0, 0);
    return;
  }
  const w = renderer.cursor_world_at(cursor.ndcX, cursor.ndcY, cursor.depth);
  // LMB attract (negative strength), RMB repel (positive).
  const sign = cursor.rmb ? +1 : -1;
  renderer.set_cursor_force(w[0], w[1], w[2], cursor.radius, sign * cursor.strength);
}

// ---------- Keyboard / mouse ----------
const keys = {};
function wireInput(boot) {
  window.addEventListener('keydown', (e) => {
    const tag = e.target && e.target.tagName;
    const inField = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
    // Always-on shortcuts (work even when typing in an input).
    if ((e.metaKey || e.ctrlKey) && (e.key === 'b' || e.key === 'B')) {
      e.preventDefault();
      document.getElementById('sidebar').classList.toggle('collapsed');
      return;
    }
    if (e.key === '?') { e.preventDefault(); $cheat.classList.toggle('hidden'); return; }
    if (e.key === ' ' && !inField) { e.preventDefault(); fitAll(boot); return; }
    if (e.key === 'Escape') {
      showModal(null); selectedIds = null; refreshColors(boot); return;
    }
    // WASD / QE / RF nav must be gated so typing in search doesn't move camera.
    if (inField) return;
    keys[e.key.toLowerCase()] = true;
  });
  window.addEventListener('keyup', (e) => { keys[e.key.toLowerCase()] = false; });
  window.addEventListener('contextmenu', (e) => {
    if (e.target === $canvas) e.preventDefault();
  });

  let dragging = false, lastX = 0, lastY = 0;
  function setNdc(e) {
    const r = $canvas.getBoundingClientRect();
    cursor.ndcX = ((e.clientX - r.left) / r.width) * 2 - 1;
    cursor.ndcY = -(((e.clientY - r.top) / r.height) * 2 - 1);
  }
  $canvas.addEventListener('pointerdown', (e) => {
    setNdc(e);
    if (e.button === 0) {
      // LMB: cursor force tool when SHIFT held; otherwise camera rotate.
      if (e.shiftKey) {
        cursor.enabled = true; cursor.lmb = true;
      } else {
        dragging = true; lastX = e.clientX; lastY = e.clientY;
      }
    } else if (e.button === 2) {
      cursor.enabled = true; cursor.rmb = true;
    }
    $canvas.setPointerCapture(e.pointerId);
    pushCursorForce();
  });
  $canvas.addEventListener('pointerup', (e) => {
    if (e.button === 0) { dragging = false; cursor.lmb = false; }
    if (e.button === 2) { cursor.rmb = false; }
    if (!cursor.lmb && !cursor.rmb) cursor.enabled = false;
    try { $canvas.releasePointerCapture(e.pointerId); } catch (_) {}
    pushCursorForce();
  });
  $canvas.addEventListener('pointermove', (e) => {
    setNdc(e);
    if (dragging) {
      const dx = e.clientX - lastX, dy = e.clientY - lastY;
      lastX = e.clientX; lastY = e.clientY;
      renderer.cam_rotate(dx * 0.005, -dy * 0.005);
    } else if (cursor.enabled) {
      pushCursorForce();
    } else {
      const idx = renderer.raycast(cursor.ndcX, cursor.ndcY);
      hoveredIdx = (idx === undefined || idx === null) ? null : idx;
      $canvas.style.cursor = hoveredIdx === null ? 'default' : 'pointer';
    }
  });
  $canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    if (cursor.enabled || e.shiftKey) {
      // Wheel adjusts cursor depth when force tool is active or shift held.
      cursor.depth = Math.max(50, cursor.depth + (e.deltaY > 0 ? -40 : 40));
      $curD.value = cursor.depth; $curDV.textContent = cursor.depth | 0;
      pushCursorForce();
    } else {
      const factor = e.deltaY > 0 ? -50 : 50;
      renderer.cam_zoom(factor);
    }
  }, { passive: false });

  $canvas.addEventListener('click', async (e) => {
    if (cursor.enabled) return; // suppress click-to-modal during force tool
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
  if (keys['w']) dz += speed; if (keys['s']) dz -= speed;
  if (keys['d']) dx += speed; if (keys['a']) dx -= speed;
  if (keys['q']) dy += speed; if (keys['e']) dy -= speed;
  if (keys['r']) dz += speed * 2; if (keys['f']) dz -= speed * 2;
  if (dx || dy || dz) renderer.cam_pan(dx, dy, dz);
}

function fitAll(boot) {
  const b = computeBounds(boot);
  renderer.cam_fit_bounds(b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]);
}
function resetCamera() { renderer.cam_reset(); }

// ---------- Sidebar ----------
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
      renderer.update_layout_options(JSON.stringify(SIM_PRESETS[preset]));
    });
  });

  $camFit  .addEventListener('click', () => fitAll(boot));
  $camReset.addEventListener('click', () => resetCamera());

  function applyFocus() {
    const z = parseFloat($focusZ.value), t = parseFloat($focusT.value);
    $focusZV.textContent = z | 0; $focusTV.textContent = t | 0;
    renderer.set_focus_plane(z, t);
  }
  $focusZ.addEventListener('input', applyFocus);
  $focusT.addEventListener('input', applyFocus);
  applyFocus();

  $curR.addEventListener('input', () => { cursor.radius = parseFloat($curR.value); $curRV.textContent = cursor.radius|0; });
  $curS.addEventListener('input', () => { cursor.strength = parseFloat($curS.value); $curSV.textContent = cursor.strength|0; });
  $curD.addEventListener('input', () => { cursor.depth = parseFloat($curD.value); $curDV.textContent = cursor.depth|0; });
  cursor.radius = parseFloat($curR.value);
  cursor.strength = parseFloat($curS.value);
  cursor.depth = parseFloat($curD.value);
}

// ---------- Floating panel (drag + collapse + persist) ----------
function wirePanel() {
  const $panel = document.getElementById('sidebar');
  const $handle = document.getElementById('panel-drag-handle');
  const $collapse = document.getElementById('panel-collapse');
  if (!$panel || !$handle) return;

  const KEY = 'graph-renderer.panel-pos';
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) || 'null');
    if (saved && typeof saved.left === 'number' && typeof saved.top === 'number') {
      $panel.style.left = `${saved.left}px`;
      $panel.style.top  = `${saved.top}px`;
    }
    if (saved && saved.collapsed) $panel.classList.add('collapsed');
  } catch (_) {}

  let dragging = false, sx = 0, sy = 0, ox = 0, oy = 0;
  $handle.addEventListener('pointerdown', (e) => {
    if (e.target === $collapse) return;
    dragging = true;
    const r = $panel.getBoundingClientRect();
    sx = e.clientX; sy = e.clientY; ox = r.left; oy = r.top;
    $handle.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  $handle.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    const nx = Math.max(0, Math.min(window.innerWidth  - 40, ox + (e.clientX - sx)));
    const ny = Math.max(0, Math.min(window.innerHeight - 40, oy + (e.clientY - sy)));
    $panel.style.left = `${nx}px`;
    $panel.style.top  = `${ny}px`;
  });
  $handle.addEventListener('pointerup', (e) => {
    if (!dragging) return;
    dragging = false;
    try { $handle.releasePointerCapture(e.pointerId); } catch (_) {}
    persist();
  });

  if ($collapse) {
    $collapse.addEventListener('click', (e) => {
      e.stopPropagation();
      $panel.classList.toggle('collapsed');
      persist();
    });
  }

  function persist() {
    const r = $panel.getBoundingClientRect();
    try {
      localStorage.setItem(KEY, JSON.stringify({
        left: r.left | 0, top: r.top | 0,
        collapsed: $panel.classList.contains('collapsed'),
      }));
    } catch (_) {}
  }
}

// ---------- Search ----------
function wireSearch(boot) {
  let regexMode = false;
  const lastSets = { search: null, include: null, exclude: null };
  const debounce = (fn, ms) => { let t = null; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };

  function regexScan(pattern) {
    const set = new Set(); let re;
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
      selectedIds = null; refreshColors(boot); return;
    }
    let final;
    if (search !== null && include !== null) {
      final = new Set(); for (const id of search) if (include.has(id)) final.add(id);
    } else if (search !== null) { final = new Set(search); }
    else if (include !== null) { final = new Set(include); }
    else { final = new Set(boot.ids); }
    if (exclude !== null) for (const id of exclude) final.delete(id);
    selectedIds = final; refreshColors(boot);
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
    regexMode = !regexMode; $regexBtn.classList.toggle('active', regexMode);
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
    </div>`;
  $modal.classList.remove('hidden');
}

// ---------- main ----------
async function main() {
  // WebGPU sanity check — storage-buffer-in-vertex-shader requires WebGPU.
  // WebGL fallback will fail at pipeline creation.
  if (!('gpu' in navigator)) {
    setStats('error: WebGPU not available. Use Chrome/Edge ≥ 113, Safari ≥ 18, or enable webgpu in your browser flags.');
    throw new Error('navigator.gpu missing');
  }
  console.log('[graph-renderer] WebGPU available, init…');

  await init('/assets/pkg/graph_renderer_bg.wasm');

  const boot = await loadBootstrap();
  console.log(`[graph-renderer] bootstrap: ${boot.nNodes} nodes, ${boot.edges.length / 2} edges, ${boot.init.numCommunities} communities`);

  syncCanvasSize();
  renderer = new WebRenderer();

  const colors = computeColors(boot);
  const sizes  = computeSizes(boot);
  console.log(`[graph-renderer] sizes range: ${Math.min(...sizes).toFixed(2)} → ${Math.max(...sizes).toFixed(2)}`);
  await renderer.init('cosmos', boot.positions, boot.edges, colors, sizes);
  renderer.resize($canvas.width, $canvas.height);

  // Fit camera to the actual data bounds so nodes land in view immediately.
  const b = computeBounds(boot);
  console.log(`[graph-renderer] data bounds: x[${b.min[0].toFixed(0)},${b.max[0].toFixed(0)}] y[${b.min[1].toFixed(0)},${b.max[1].toFixed(0)}] z[${b.min[2].toFixed(0)},${b.max[2].toFixed(0)}]`);
  renderer.cam_fit_bounds(b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2]);

  // Wave 2: live force sim. Builds GpuForceLayout against the renderer's
  // device + shared positions buffer.
  try {
    renderer.init_layout(boot.edges, JSON.stringify(SIM_PRESETS[state.preset]));
    console.log('[graph-renderer] live force sim active');
  } catch (e) {
    console.warn('init_layout failed (force sim disabled):', e);
  }

  setStats(
    `${boot.nNodes.toLocaleString()} nodes • ` +
    `${Number(boot.init.nEdges).toLocaleString()} edges • ` +
    `${boot.init.numCommunities} communities`
  );

  wirePanel();
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
    try { renderer.step(); } catch (e) { console.error(e); }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

main().catch((e) => {
  console.error(e);
  setStats(`error: ${e.message || e}`);
});
