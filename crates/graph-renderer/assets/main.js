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
const $focusBlur  = document.getElementById('focus-blur');
const $focusBlurV = document.getElementById('focus-blur-val');
const $focusMaxCoc  = document.getElementById('focus-max-coc');
const $focusMaxCocV = document.getElementById('focus-max-coc-val');
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

  // Discard the server's 2D ring seeding entirely — it traps the sim in a
  // ring shape. Seed uniformly in a 3D ball via rejection sampling so the
  // force sim has full freedom to find the natural layout.
  const positions3 = new Float32Array(nNodes * 3);
  const radius = Math.max(200, Math.cbrt(nNodes) * 60);
  for (let i = 0; i < nNodes; i++) {
    let x, y, z, r2;
    do {
      x = Math.random() * 2 - 1;
      y = Math.random() * 2 - 1;
      z = Math.random() * 2 - 1;
      r2 = x*x + y*y + z*z;
    } while (r2 > 1.0 || r2 < 1e-6);
    positions3[i * 3 + 0] = x * radius;
    positions3[i * 3 + 1] = y * radius;
    positions3[i * 3 + 2] = z * radius;
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
  // Energy-halt readback is disabled by default — the WASM map_async
  // pattern re-enters itself and panics ("Buffer is already mapped").
  // TODO: rework readback to use a deferred poll-only path before re-enabling.
  energy_threshold: 0.0,
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
  // Cursor force is a perturbation — make sure the sim is awake to feel it.
  try { renderer.sim_wake(); } catch (_) {}
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
      // Orbit-style: drag right → graph appears to spin to the right
      // (camera orbits counter-clockwise → yaw decreases). Pitch also
      // inverted for natural drag-the-graph feel.
      renderer.cam_rotate(-dx * 0.005, dy * 0.005);
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
    if (keys['f']) {
      // Microscope focus knob: F + scroll moves the focal plane along the
      // camera's view ray (i.e. focus distance from camera, not world Z).
      const z = parseFloat($focusZ.value);
      const min = parseFloat($focusZ.min), max = parseFloat($focusZ.max);
      const dz = (e.deltaY > 0 ? -1 : 1) * 20;
      const nz = Math.max(min, Math.min(max, z + dz));
      $focusZ.value = nz;
      $focusZV.textContent = nz | 0;
      state.focus_z = nz;
      const t = parseFloat($focusT.value);
      renderer.set_focus_plane(nz, t);
      return;
    }
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

  // Cosmograph-style: double-click on canvas exits selection-focus (clears
  // the dim-others state). Keeps DoF / focal band untouched — they're a
  // separate concept (camera-orthogonal depth-of-field).
  $canvas.addEventListener('dblclick', (e) => {
    if (cursor.enabled) return;
    showModal(null);
    selectedIds = null;
    refreshColors(boot);
  });
}

function applyKeyboardCamera(dt) {
  const speed = (keys['shift'] ? 5 : 1) * 400 * dt;
  let dx = 0, dy = 0, dz = 0;
  // Drag-the-graph semantics (Cosmograph-style): pressing a direction key
  // moves the GRAPH that way, so the camera moves the opposite way. This
  // matches the user's expectation more than FPS-style "press d, camera
  // goes right" because graph apps feel like manipulating the canvas.
  if (keys['w']) dz += speed; if (keys['s']) dz -= speed;   // forward/back unchanged
  if (keys['d']) dx -= speed; if (keys['a']) dx += speed;   // FLIPPED
  if (keys['q']) dy -= speed; if (keys['e']) dy += speed;   // FLIPPED
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
      try { renderer.sim_wake(); } catch (_) {}
    });
  });

  $camFit  .addEventListener('click', () => fitAll(boot));
  $camReset.addEventListener('click', () => resetCamera());

  function applyFocus() {
    const z = parseFloat($focusZ.value), t = parseFloat($focusT.value);
    $focusZV.textContent = z | 0; $focusTV.textContent = t | 0;
    state.focus_z = z; state.focus_thickness = t;
    renderer.set_focus_plane(z, t);
  }
  function applyDof() {
    const b = parseFloat($focusBlur.value), m = parseFloat($focusMaxCoc.value);
    $focusBlurV.textContent = b.toFixed(3);
    $focusMaxCocV.textContent = m | 0;
    if (renderer.set_dof_params) renderer.set_dof_params(b, m);
  }
  $focusZ.addEventListener('input', applyFocus);
  $focusT.addEventListener('input', applyFocus);
  $focusBlur.addEventListener('input', applyDof);
  $focusMaxCoc.addEventListener('input', applyDof);
  applyFocus();
  applyDof();

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

// ---------- Modal: rich typed badges + click-to-filter ----------
//
// Restores the metadata UX from vault-graph-cosmos.py: per-field type
// detection (wikilinks, URLs, dates, tags, status pills, lists), each
// badge is clickable to toggle a (field, value) filter that drills the
// graph down via `selectedIds`.
//
// FILTER INDEX NOTE: the inverted index `fieldIndex: Map<field, Map<value,
// Set<nodeId>>>` is built **lazily** — only nodes whose modal has been
// opened contribute. A server-side `/graph/field_index` endpoint covering
// the full vault is the proper follow-up.

const $filterChips = document.getElementById('filter-chips');
let bootRef = null;          // captured in main() so badge handlers can read ids
let currentMeta = null;      // meta of currently-pinned modal node
const fieldIndex = new Map();           // Map<field, Map<value, Set<nodeId>>>
const activeFieldFilters = new Map();   // Map<field, Set<value>>

const _wikilinkRe = /\[\[[^\]]+\]\]/;
const _wikiSplitRe = /\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g;
const _urlRe = /^https?:\/\//i;
const _dateRe = /^\d{4}-\d{2}-\d{2}$/;
const KNOWN_STATUS = new Set(['active', 'draft', 'needs-review', 'needs-fetch',
                              'archived', 'failed', 'done']);

function _isFilterActive(field, value) {
  const s = activeFieldFilters.get(field);
  return !!(s && s.has(String(value)));
}

function _addToFieldIndex(field, value, nid) {
  if (typeof value !== 'string' && typeof value !== 'number' && typeof value !== 'boolean') return;
  const v = String(value); if (!v) return;
  let f = fieldIndex.get(field);
  if (!f) { f = new Map(); fieldIndex.set(field, f); }
  let bucket = f.get(v);
  if (!bucket) { bucket = new Set(); f.set(v, bucket); }
  bucket.add(nid);
}
function _ingestMetaIntoIndex(meta) {
  if (!meta || !meta.id) return {};
  const nid = meta.id;
  for (const t of (meta.tags || [])) _addToFieldIndex('tags', t, nid);
  if (meta.doctype) _addToFieldIndex('doctype', meta.doctype, nid);
  if (meta.folder)  _addToFieldIndex('folder',  meta.folder,  nid);
  let fm = {};
  try { fm = meta.frontmatterJson ? JSON.parse(meta.frontmatterJson) : {}; }
  catch (_) { fm = {}; }
  for (const [k, v] of Object.entries(fm)) {
    if (k === 'tags') continue;
    if (Array.isArray(v)) for (const item of v) _addToFieldIndex(k, item, nid);
    else _addToFieldIndex(k, v, nid);
  }
  return fm;
}

// ---------- Field-type detection ----------
function inferType(_field, value) {
  if (value === null || value === undefined) return 'scalar';
  if (Array.isArray(value)) {
    if (value.length === 0) return 'list';
    let hasWiki = false, hasUrl = false;
    for (const it of value) {
      if (typeof it === 'string') {
        if (_wikilinkRe.test(it)) hasWiki = true;
        if (_urlRe.test(it))      hasUrl  = true;
      }
    }
    if (hasWiki) return 'wikilink';
    if (hasUrl)  return 'url';
    return 'list';
  }
  if (typeof value === 'object') return 'object';
  if (typeof value === 'string') {
    if (_wikilinkRe.test(value)) return 'wikilink';
    if (_urlRe.test(value))      return 'url';
    if (_dateRe.test(value.trim())) return 'date';
    if (KNOWN_STATUS.has(value.trim())) return 'status';
    if (value.length > 120) return 'long_text';
    return 'scalar';
  }
  return 'scalar';
}

// ---------- Badge factories ----------
function makeBadge(field, value, displayText, extraClass) {
  const span = document.createElement('span');
  const active = _isFilterActive(field, value);
  span.className = (extraClass || 'badge') + (active ? ' active' : '');
  span.dataset.field = field;
  span.dataset.value = String(value);
  span.textContent = displayText !== undefined ? displayText : String(value);
  span.title = active ? `Remove filter ${field}=${value}` : `Filter to ${field}=${value}`;
  return span;
}
function makeTagBadge(value) { return makeBadge('tags', value, value, 'tag'); }
function makeStatusBadge(field, value) {
  const v = String(value).trim();
  let cls = 'status';
  if (KNOWN_STATUS.has(v)) cls += ' s-' + v;
  return makeBadge(field, v, v, cls);
}
function makeDateBadge(field, value) { return makeBadge(field, value, value, 'date'); }
function makeUrlChip(value) {
  const url = String(value);
  let domain = url; try { domain = new URL(url).hostname; } catch (_) {}
  const a = document.createElement('a');
  a.className = 'url'; a.href = url; a.target = '_blank'; a.rel = 'noopener noreferrer';
  a.textContent = domain; a.title = `Open ${domain}`;
  return a;
}
function _resolveWikilink(target) {
  if (!bootRef) return null;
  const idx = bootRef.idToIdx;
  if (idx.has(target)) return target;
  const base = target.split('/').pop();
  if (idx.has(base)) return base;
  const noExt = base.replace(/\.md$/, '');
  if (idx.has(noExt)) return noExt;
  for (const id of bootRef.ids) {
    const idBase = id.split('/').pop();
    if (idBase === base || idBase === noExt) return id;
  }
  return null;
}
function _parseWikilinks(s) {
  if (typeof s !== 'string') return [];
  const out = []; let m; _wikiSplitRe.lastIndex = 0;
  while ((m = _wikiSplitRe.exec(s)) !== null) {
    out.push({ target: m[1].trim(), alias: (m[2] || '').trim() || null });
  }
  return out;
}
function makeWikilinkChip(target, alias) {
  const resolved = _resolveWikilink(target);
  const span = document.createElement('span');
  span.className = 'wikilink' + (resolved ? '' : ' unresolved');
  span.dataset.action = 'navigate';
  span.dataset.target = resolved || target;
  span.textContent = alias || target;
  span.title = resolved ? `Open ${target}` : `(unresolved) ${target}`;
  return span;
}
function renderWikilinkChips(_field, value) {
  const out = [];
  const handle = (s) => {
    if (typeof s !== 'string') return;
    if (_wikilinkRe.test(s)) for (const w of _parseWikilinks(s)) out.push(makeWikilinkChip(w.target, w.alias));
    else if (s.trim()) out.push(makeWikilinkChip(s.trim(), null));
  };
  if (Array.isArray(value)) for (const v of value) handle(v);
  else handle(value);
  return out;
}
function renderUrlChips(_field, value) {
  const arr = Array.isArray(value) ? value : [value];
  const out = [];
  for (const u of arr) if (typeof u === 'string' && _urlRe.test(u)) out.push(makeUrlChip(u));
  return out;
}
function renderListBadges(field, value) {
  const arr = Array.isArray(value) ? value : [value];
  const out = [];
  for (const v of arr) {
    if (v === null || v === undefined) continue;
    const s = String(v).trim(); if (!s) continue;
    out.push(makeBadge(field, s, s));
  }
  return out;
}
function renderEntityBadges(field, values) {
  const PREFIX = { person: '[person]', org: '[org]', project: '[project]',
                   company: '[org]', team: '[team]', tool: '[tool]', system: '[system]' };
  const arr = Array.isArray(values) ? values : [values];
  const out = [];
  for (const item of arr) {
    let name, type;
    if (typeof item === 'string') { name = item; type = null; }
    else if (item && typeof item === 'object') {
      name = item.name || item.value || item.label || JSON.stringify(item);
      type = item.type || item.kind || null;
    } else continue;
    name = String(name).trim(); if (!name) continue;
    const display = type ? `${PREFIX[String(type).toLowerCase()] || `[${type}]`} ${name}` : name;
    out.push(makeBadge(field, name, display));
  }
  return out;
}
function renderAuthorBadges(field, value) {
  let names = [];
  if (Array.isArray(value)) {
    for (const v of value) if (typeof v === 'string') names.push(...v.split(',').map(s => s.trim()).filter(Boolean));
  } else if (typeof value === 'string') {
    names = value.split(',').map(s => s.trim()).filter(Boolean);
  }
  return names.map(n => makeBadge(field, n, n));
}

// Special-case (doctype-aware) renderers; null = fall through to inferType.
function _specialRender(doctype, key, value, fm) {
  if (key === 'authors' && doctype === 'literature') return renderAuthorBadges(key, value);
  if (key === 'citekey' && doctype === 'literature') {
    const src = fm && fm.source;
    if (typeof src === 'string' && _urlRe.test(src)) {
      const a = document.createElement('a');
      a.className = 'url'; a.href = src; a.target = '_blank'; a.rel = 'noopener noreferrer';
      a.textContent = String(value); a.title = `Open source: ${src}`;
      return [a];
    }
    return [makeBadge(key, String(value), String(value))];
  }
  if (key === 'related')    return renderWikilinkChips(key, value);
  if (key === 'entities')   return renderEntityBadges(key, value);
  if (key === 'key_topics') return renderListBadges(key, value);
  if (key === 'status')     return [makeStatusBadge(key, value)];
  return null;
}

// Returns { kind: 'badges', els } or { kind: 'dd', el }.
function renderField(key, value, doctype, fm) {
  const sp = _specialRender(doctype, key, value, fm);
  if (sp) return { kind: 'badges', els: sp };
  const t = inferType(key, value);
  switch (t) {
    case 'wikilink': return { kind: 'badges', els: renderWikilinkChips(key, value) };
    case 'url':      return { kind: 'badges', els: renderUrlChips(key, value) };
    case 'date':     return { kind: 'badges', els: [makeDateBadge(key, value)] };
    case 'status':   return { kind: 'badges', els: [makeStatusBadge(key, value)] };
    case 'list':     return { kind: 'badges', els: renderListBadges(key, value) };
    case 'scalar':   return { kind: 'badges', els: [makeBadge(key, String(value), String(value))] };
    case 'object': {
      const dd = document.createElement('dd');
      let s = JSON.stringify(value); if (s.length > 120) s = s.slice(0, 117) + '...';
      dd.textContent = s; return { kind: 'dd', el: dd };
    }
    case 'long_text':
    default: {
      const dd = document.createElement('dd');
      dd.textContent = String(value);
      return { kind: 'dd', el: dd };
    }
  }
}

// ---------- Modal render ----------
function showModal(meta) {
  if (!meta) { $modal.classList.add('hidden'); currentMeta = null; return; }
  const fm = _ingestMetaIntoIndex(meta) || {};
  currentMeta = meta;
  while ($modal.firstChild) $modal.removeChild($modal.firstChild);

  const header = document.createElement('div');
  header.className = 'modal-header';
  const h2 = document.createElement('h2');
  h2.textContent = meta.title || meta.path || '?';
  const closeBtn = document.createElement('button');
  closeBtn.type = 'button'; closeBtn.className = 'modal-close';
  closeBtn.textContent = '\u2715';
  closeBtn.addEventListener('click', (ev) => { ev.stopPropagation(); showModal(null); });
  header.append(h2, closeBtn);
  $modal.appendChild(header);

  const path = document.createElement('div');
  path.className = 'modal-path';
  path.textContent = meta.path || '';
  $modal.appendChild(path);

  const tags = meta.tags || [];
  if (tags.length) {
    const row = document.createElement('div');
    row.className = 'modal-tags';
    for (const t of tags) row.appendChild(makeTagBadge(t));
    $modal.appendChild(row);
  }

  if (meta.doctype || meta.folder) {
    const row = document.createElement('div');
    row.className = 'field-row';
    if (meta.doctype) {
      const pill = document.createElement('span'); pill.className = 'field-name-pill';
      pill.textContent = 'doctype'; row.appendChild(pill);
      row.appendChild(makeBadge('doctype', meta.doctype, meta.doctype));
    }
    if (meta.folder) {
      const pill = document.createElement('span'); pill.className = 'field-name-pill';
      pill.textContent = 'folder'; row.appendChild(pill);
      row.appendChild(makeBadge('folder', meta.folder, meta.folder));
    }
    $modal.appendChild(row);
  }

  const dl = document.createElement('dl');
  dl.className = 'modal-frontmatter';
  let dlHasContent = false;
  for (const [key, value] of Object.entries(fm)) {
    if (key === 'tags' || value === null || value === undefined) continue;
    const r = renderField(key, value, meta.doctype, fm);
    if (!r) continue;
    if (r.kind === 'badges') {
      if (!r.els || r.els.length === 0) continue;
      const row = document.createElement('div');
      row.className = 'field-row';
      const pill = document.createElement('span');
      pill.className = 'field-name-pill'; pill.textContent = key;
      row.appendChild(pill);
      for (const el of r.els) row.appendChild(el);
      $modal.appendChild(row);
    } else if (r.kind === 'dd' && r.el) {
      const dt = document.createElement('dt'); dt.textContent = key;
      dl.append(dt, r.el); dlHasContent = true;
    }
  }
  if (dlHasContent) $modal.appendChild(dl);

  const metrics = document.createElement('div');
  metrics.className = 'modal-metrics';
  const addMetric = (label, val) => {
    const m = document.createElement('span'); m.className = 'metric';
    const l = document.createElement('label'); l.textContent = label;
    m.append(l, document.createTextNode(' ' + val));
    metrics.appendChild(m);
  };
  addMetric('degree', meta.degree ?? '');
  addMetric('in', meta.indegree ?? '');
  addMetric('out', meta.outdegree ?? '');
  addMetric('pagerank', (meta.pagerank ?? 0).toFixed(4));
  addMetric('betweenness', (meta.betweenness ?? 0).toFixed(4));
  addMetric('kcore', meta.kcore ?? '');
  addMetric('community', meta.community ?? '');
  addMetric('wcc', meta.wcc ?? '');
  $modal.appendChild(metrics);

  $modal.classList.remove('hidden');
}

// ---------- Filter chip strip ----------
function renderFilterChips() {
  while ($filterChips.firstChild) $filterChips.removeChild($filterChips.firstChild);
  if (activeFieldFilters.size === 0) { $filterChips.classList.add('hidden'); return; }
  $filterChips.classList.remove('hidden');
  let totalValues = 0;
  for (const [field, valueSet] of activeFieldFilters) {
    if (!valueSet || valueSet.size === 0) continue;
    for (const value of valueSet) {
      const chip = document.createElement('span'); chip.className = 'fchip';
      const f = document.createElement('span'); f.className = 'fchip-field'; f.textContent = field + ':';
      const v = document.createElement('span'); v.textContent = ' ' + value + ' ';
      const x = document.createElement('span'); x.className = 'fchip-x'; x.textContent = '\u2715';
      x.addEventListener('click', (ev) => { ev.stopPropagation(); toggleFieldFilter(field, value); });
      chip.append(f, v, x);
      $filterChips.appendChild(chip);
      totalValues++;
    }
  }
  if (totalValues >= 2) {
    const clear = document.createElement('button');
    clear.type = 'button'; clear.className = 'fchip-clear';
    clear.textContent = 'clear filters';
    clear.addEventListener('click', (ev) => {
      ev.stopPropagation();
      activeFieldFilters.clear();
      applyFieldFilters();
      renderFilterChips();
      if (currentMeta) showModal(currentMeta);
    });
    $filterChips.appendChild(clear);
  }
}

function toggleFieldFilter(field, value) {
  const v = String(value);
  let bucket = activeFieldFilters.get(field);
  if (!bucket) { bucket = new Set(); activeFieldFilters.set(field, bucket); }
  if (bucket.has(v)) bucket.delete(v); else bucket.add(v);
  if (bucket.size === 0) activeFieldFilters.delete(field);
  applyFieldFilters();
  renderFilterChips();
  if (currentMeta) showModal(currentMeta); // refresh badge .active state
}

// Recompute selectedIds from active filters: within-field OR, cross-field AND.
function applyFieldFilters() {
  if (!bootRef) return;
  if (activeFieldFilters.size === 0) {
    selectedIds = null; refreshColors(bootRef); return;
  }
  const perField = [];
  for (const [field, valueSet] of activeFieldFilters) {
    if (!valueSet || valueSet.size === 0) continue;
    const fmap = fieldIndex.get(field);
    if (!fmap) { selectedIds = new Set(); refreshColors(bootRef); return; }
    const merged = new Set();
    for (const v of valueSet) {
      const bucket = fmap.get(v); if (!bucket) continue;
      for (const id of bucket) merged.add(id);
    }
    if (merged.size === 0) { selectedIds = new Set(); refreshColors(bootRef); return; }
    perField.push(merged);
  }
  if (perField.length === 0) { selectedIds = null; refreshColors(bootRef); return; }
  perField.sort((a, b) => a.size - b.size);
  const out = new Set();
  outer: for (const id of perField[0]) {
    for (let i = 1; i < perField.length; i++) if (!perField[i].has(id)) continue outer;
    out.add(id);
  }
  selectedIds = out;
  refreshColors(bootRef);
}

// Wikilink navigation — open the resolved node's modal and nudge the camera.
async function navigateToNode(target) {
  if (!bootRef) return;
  const id = _resolveWikilink(target);
  if (!id) return;
  try {
    const meta = await fetchProto(`/node/${encodeURIComponent(id)}`, NodeMeta);
    showModal(meta);
    const i = bootRef.idToIdx.get(id);
    if (i !== undefined && renderer && renderer.cam_fit_bounds) {
      const x = bootRef.positions[i*3], y = bootRef.positions[i*3+1], z = bootRef.positions[i*3+2];
      const r = 80;
      try { renderer.cam_fit_bounds(x-r, y-r, z-r, x+r, y+r, z+r); } catch (_) {}
    }
  } catch (e) { console.error(e); }
}

// Single delegated badge-click handler (modal + chip strip). All paths
// stopPropagation so the canvas/background-click logic doesn't see them.
document.addEventListener('click', (ev) => {
  const url = ev.target.closest && ev.target.closest('a.url');
  if (url) { ev.stopPropagation(); return; }
  const wl = ev.target.closest && ev.target.closest('.wikilink');
  if (wl) {
    ev.stopPropagation(); ev.preventDefault();
    const target = wl.dataset && wl.dataset.target;
    if (target) navigateToNode(target);
    return;
  }
  const badge = ev.target.closest && ev.target.closest('.badge, .tag, .status, .date');
  if (!badge) return;
  if (badge.closest('#filter-chips')) return; // chip-strip x already handled it
  const field = badge.dataset && badge.dataset.field;
  const value = badge.dataset && badge.dataset.value;
  if (!field || value === undefined) return;
  ev.stopPropagation(); ev.preventDefault();
  toggleFieldFilter(field, value);
}, true);

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
  bootRef = boot;
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

  // Static prefix for the stats line; the frame loop appends a dynamic
  // suffix (settled indicator / live KE) without re-stringifying these.
  const statsPrefix =
    `${boot.nNodes.toLocaleString()} nodes • ` +
    `${Number(boot.init.nEdges).toLocaleString()} edges • ` +
    `${boot.init.numCommunities} communities`;
  setStats(statsPrefix);

  wirePanel();
  wireSidebar(boot);
  wireSearch(boot);
  wireInput(boot);

  const ro = new ResizeObserver(syncCanvasSize);
  ro.observe($canvas);
  window.addEventListener('resize', syncCanvasSize);

  let lastT = performance.now();
  let lastStatsT = 0;
  function frame() {
    const now = performance.now();
    const dt = Math.min(0.1, (now - lastT) / 1000);
    lastT = now;
    applyKeyboardCamera(dt);
    try { renderer.step(); } catch (e) { console.error(e); }
    // Throttle stats updates to ~4Hz so we don't thrash layout.
    if (now - lastStatsT > 250) {
      lastStatsT = now;
      try {
        const halted = renderer.sim_halted();
        const ke = renderer.sim_max_ke();
        setStats(`${statsPrefix} • ${halted ? 'settled' : `KE=${ke.toFixed(2)}`}`);
      } catch (_) { /* sim_halted may not exist before a rebuild */ }
    }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

main().catch((e) => {
  console.error(e);
  setStats(`error: ${e.message || e}`);
});
