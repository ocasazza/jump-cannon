// graph-renderer / main.js
//
// Talks to graph-api over HTTP. Wire format split:
//   - bulk numeric (positions, edges, metrics) → raw Float32Array / Uint32Array
//   - structured (init, node metadata, search) → protobuf via protobufjs
//   - id list (/graph/ids) → JSON, fetched once at startup
//
// Rendering: three.js. InstancedMesh for nodes, LineSegments for edges.
// Camera: OrbitControls + additive WASD/QE/RF keyboard 6DoF.
// Layout sim is currently NOT live — server hands us static 2D positions
// and we synthesize Z. Wave 2 will replace positionsBuf updates with the
// graph-layouts WASM GPU compute output.

import * as THREE from 'https://esm.sh/three@0.160.0';
import { OrbitControls } from 'https://esm.sh/three@0.160.0/examples/jsm/controls/OrbitControls.js';
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

// ---------- Color helpers (0-1 floats for three.js) ----------
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

// ---------- 1. Bootstrap ----------
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

  // Init.palette is flat [r,g,b,r,g,b,...] of normalized floats.
  const palette = [];
  for (let i = 0; i + 2 < init.palette.length; i += 3) {
    palette.push([init.palette[i], init.palette[i + 1], init.palette[i + 2]]);
  }

  const nNodes = Number(init.nNodes);
  const idToIdx = new Map();
  for (let i = 0; i < nNodes; i++) idToIdx.set(ids[i], i);

  // Server gives us 2D; promote to 3D with a small random Z spread until
  // graph-layouts WASM hands us 3D positions in Wave 2.
  const positionsBuf = new Float32Array(nNodes * 3);
  for (let i = 0; i < nNodes; i++) {
    positionsBuf[i * 3 + 0] = positions[i * 2 + 0];
    positionsBuf[i * 3 + 1] = positions[i * 2 + 1];
    positionsBuf[i * 3 + 2] = (Math.random() - 0.5) * 200;
  }

  return { init, ids, idToIdx, edges, positionsBuf, palette, metrics, bounds, nNodes };
}

// ---------- 2. three.js scene ----------
const SIM_PRESETS = {
  fast:     {},
  balanced: {},
  pretty:   {},
};

const state = {
  sizeBy: 'degree',
  sizeMul: 1.0,
  colorBy: 'community',
  preset: 'balanced',
};

let scene, camera, controls, renderer;
let nodeMesh, edgeMesh;
let raycaster, mousePos;
let hoveredIdx = null;
let selectedIds = null;     // Set<string> | null — null = nothing selected (all "normal")
const dummy = new THREE.Object3D();

function initThree() {
  scene = new THREE.Scene();
  scene.background = new THREE.Color(0x0d0d10);

  const w = $canvas.clientWidth || window.innerWidth;
  const h = $canvas.clientHeight || window.innerHeight;
  camera = new THREE.PerspectiveCamera(60, w / h, 0.1, 200000);
  camera.position.set(0, 0, 1500);

  renderer = new THREE.WebGLRenderer({ antialias: true, canvas: $canvas });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, 1.5));
  renderer.setSize(w, h, false);

  controls = new OrbitControls(camera, $canvas);
  controls.enableDamping = true;
  controls.dampingFactor = 0.1;
  controls.target.set(0, 0, 0);

  raycaster = new THREE.Raycaster();
  raycaster.params.Points = { threshold: 4 };
  mousePos = new THREE.Vector2();

  // Some fill light so the spheres aren't flat — though MeshBasicMaterial
  // ignores lights, MeshLambertMaterial uses them.
  scene.add(new THREE.AmbientLight(0xffffff, 0.7));
  const dl = new THREE.DirectionalLight(0xffffff, 0.6);
  dl.position.set(1, 1, 1);
  scene.add(dl);

  window.addEventListener('resize', onResize);
}

function onResize() {
  const w = $canvas.clientWidth || window.innerWidth;
  const h = $canvas.clientHeight || window.innerHeight;
  renderer.setSize(w, h, false);
  camera.aspect = w / h;
  camera.updateProjectionMatrix();
}

function buildNodeMesh(boot) {
  const geom = new THREE.SphereGeometry(1, 8, 6);
  const mat  = new THREE.MeshLambertMaterial({ vertexColors: true });
  nodeMesh = new THREE.InstancedMesh(geom, mat, boot.nNodes);
  nodeMesh.instanceColor = new THREE.InstancedBufferAttribute(
    new Float32Array(boot.nNodes * 3), 3
  );
  scene.add(nodeMesh);
  updateNodeSizes(boot);
  updateNodeColors(boot);
}

function buildEdgeMesh(boot) {
  const nEdges = boot.edges.length / 2;
  const positions = new Float32Array(nEdges * 2 * 3);
  for (let i = 0; i < nEdges; i++) {
    const a = boot.edges[i * 2];
    const b = boot.edges[i * 2 + 1];
    positions[i * 6 + 0] = boot.positionsBuf[a * 3 + 0];
    positions[i * 6 + 1] = boot.positionsBuf[a * 3 + 1];
    positions[i * 6 + 2] = boot.positionsBuf[a * 3 + 2];
    positions[i * 6 + 3] = boot.positionsBuf[b * 3 + 0];
    positions[i * 6 + 4] = boot.positionsBuf[b * 3 + 1];
    positions[i * 6 + 5] = boot.positionsBuf[b * 3 + 2];
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute('position', new THREE.BufferAttribute(positions, 3));
  const m = new THREE.LineBasicMaterial({
    color: 0x555562, transparent: true, opacity: 0.4,
  });
  edgeMesh = new THREE.LineSegments(g, m);
  scene.add(edgeMesh);
}

function nodeSizeAt(boot, idx) {
  const arr = boot.metrics[state.sizeBy];
  const { min, max } = boot.bounds[state.sizeBy];
  const span = max - min;
  const mul = state.sizeMul;
  if (!arr || span <= 0) return 2 * mul;
  const t = Math.sqrt((arr[idx] - min) / span);
  return (1 + t * 9) * mul;
}

function nodeColorAt(boot, idx) {
  const key = state.colorBy;
  if (key === 'community' || key === 'wcc') {
    const arr = boot.metrics[key];
    return paletteColor01(boot.palette, arr[idx] | 0);
  }
  if (key === 'folder') {
    // TODO: per-node folder via NodeMeta or bulk endpoint; fall back to community.
    const arr = boot.metrics.community;
    return paletteColor01(boot.palette, arr[idx] | 0);
  }
  const arr = boot.metrics[key];
  const { min, max } = boot.bounds[key];
  const span = max - min;
  if (span <= 0) return gradient01(0);
  return gradient01((arr[idx] - min) / span);
}

function updateNodeSizes(boot) {
  for (let i = 0; i < boot.nNodes; i++) {
    const s = nodeSizeAt(boot, i);
    dummy.position.set(
      boot.positionsBuf[i * 3 + 0],
      boot.positionsBuf[i * 3 + 1],
      boot.positionsBuf[i * 3 + 2],
    );
    dummy.scale.setScalar(s);
    dummy.updateMatrix();
    nodeMesh.setMatrixAt(i, dummy.matrix);
  }
  nodeMesh.instanceMatrix.needsUpdate = true;
}

function updateNodeColors(boot) {
  const ca = nodeMesh.instanceColor.array;
  const dim = (selectedIds !== null);
  for (let i = 0; i < boot.nNodes; i++) {
    const c = nodeColorAt(boot, i);
    let mul = 1.0;
    if (dim) {
      mul = selectedIds.has(boot.ids[i]) ? 1.0 : 0.18;
    }
    ca[i * 3 + 0] = c[0] * mul;
    ca[i * 3 + 1] = c[1] * mul;
    ca[i * 3 + 2] = c[2] * mul;
  }
  nodeMesh.instanceColor.needsUpdate = true;
}

function refreshAccessors(boot) {
  updateNodeSizes(boot);
  updateNodeColors(boot);
}

// ---------- 3. Camera helpers ----------
const initialCam = { pos: new THREE.Vector3(0, 0, 1500), target: new THREE.Vector3(0, 0, 0) };

function computeBoundingSphere(boot) {
  let cx = 0, cy = 0, cz = 0;
  for (let i = 0; i < boot.nNodes; i++) {
    cx += boot.positionsBuf[i * 3 + 0];
    cy += boot.positionsBuf[i * 3 + 1];
    cz += boot.positionsBuf[i * 3 + 2];
  }
  cx /= boot.nNodes; cy /= boot.nNodes; cz /= boot.nNodes;
  let r2 = 0;
  for (let i = 0; i < boot.nNodes; i++) {
    const dx = boot.positionsBuf[i * 3 + 0] - cx;
    const dy = boot.positionsBuf[i * 3 + 1] - cy;
    const dz = boot.positionsBuf[i * 3 + 2] - cz;
    const d2 = dx * dx + dy * dy + dz * dz;
    if (d2 > r2) r2 = d2;
  }
  return { center: new THREE.Vector3(cx, cy, cz), radius: Math.sqrt(r2) || 1 };
}

function fitAll(boot) {
  const { center, radius } = computeBoundingSphere(boot);
  const fov = camera.fov * Math.PI / 180;
  const dist = (radius * 1.4) / Math.sin(fov / 2);
  const dir = new THREE.Vector3(0, 0, 1);
  camera.position.copy(center).addScaledVector(dir, dist);
  controls.target.copy(center);
  controls.update();
}

function resetCamera() {
  camera.position.copy(initialCam.pos);
  controls.target.copy(initialCam.target);
  controls.update();
}

// ---------- 4. Keyboard 6DoF (additive to OrbitControls) ----------
const keys = {};
window.addEventListener('keydown', (e) => {
  // ignore when typing in input/search
  const tag = e.target && e.target.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
  keys[e.key.toLowerCase()] = true;
});
window.addEventListener('keyup', (e) => { keys[e.key.toLowerCase()] = false; });

function applyKeyboardCamera(dt) {
  const speed = (keys['shift'] ? 5 : 1) * 400 * dt;
  const fwd = new THREE.Vector3();
  camera.getWorldDirection(fwd);
  const right = new THREE.Vector3().crossVectors(fwd, camera.up).normalize();
  const up    = new THREE.Vector3().crossVectors(right, fwd).normalize();

  let moved = false;
  const move = (v, s) => {
    camera.position.addScaledVector(v, s);
    controls.target.addScaledVector(v, s);
    moved = true;
  };
  if (keys['w']) move(fwd, speed);
  if (keys['s']) move(fwd, -speed);
  if (keys['a']) move(right, -speed);
  if (keys['d']) move(right, speed);
  if (keys['q']) move(up, speed);
  if (keys['e']) move(up, -speed);
  if (keys['r']) move(fwd, speed * 2);
  if (keys['f']) move(fwd, -speed * 2);
  if (moved) controls.update();
}

// ---------- 5. Hover + click ----------
function wireRaycast(boot) {
  $canvas.addEventListener('pointermove', (e) => {
    const r = $canvas.getBoundingClientRect();
    mousePos.x = ((e.clientX - r.left) / r.width) * 2 - 1;
    mousePos.y = -((e.clientY - r.top) / r.height) * 2 + 1;
    raycaster.setFromCamera(mousePos, camera);
    const hits = raycaster.intersectObject(nodeMesh);
    if (hits.length) {
      hoveredIdx = hits[0].instanceId;
      $canvas.style.cursor = 'pointer';
    } else {
      hoveredIdx = null;
      $canvas.style.cursor = 'default';
    }
  });

  $canvas.addEventListener('click', async () => {
    if (hoveredIdx == null) { showModal(null); return; }
    const id = boot.ids[hoveredIdx];
    try {
      const meta = await fetchProto(`/node/${encodeURIComponent(id)}`, NodeMeta);
      showModal(meta);
    } catch (e) { console.error(e); }
  });
}

// ---------- 6. Animation loop ----------
let lastT = performance.now();
function animate() {
  requestAnimationFrame(animate);
  const now = performance.now();
  const dt = Math.min(0.1, (now - lastT) / 1000);
  lastT = now;
  applyKeyboardCamera(dt);
  controls.update();
  // TODO Wave 2: pull updated positionsBuf from graph-layouts WASM compute,
  // re-write nodeMesh instance matrices and edgeMesh `position` attribute.
  renderer.render(scene, camera);
}

// ---------- 7. Sidebar controls ----------
function wireSidebar(boot) {
  $sizeBy.addEventListener('change', () => {
    state.sizeBy = $sizeBy.value;
    refreshAccessors(boot);
  });
  $sizeMul.addEventListener('input', () => {
    state.sizeMul = parseFloat($sizeMul.value);
    $sizeMulV.textContent = state.sizeMul.toFixed(2);
    refreshAccessors(boot);
  });
  $colorBy.addEventListener('change', () => {
    state.colorBy = $colorBy.value;
    refreshAccessors(boot);
  });

  $simBtns.forEach((btn) => {
    btn.addEventListener('click', () => {
      const preset = btn.dataset.preset;
      if (!SIM_PRESETS[preset]) return;
      state.preset = preset;
      $simBtns.forEach((b) => b.classList.toggle('active', b === btn));
      // TODO Wave 2: wire to graph-layouts WASM GPU compute (preset → sim params).
    });
  });

  $camFit.addEventListener('click', () => fitAll(boot));
  $camReset.addEventListener('click', () => resetCamera());

  // Cmd/Ctrl + B → toggle sidebar
  // ? → toggle cheatsheet
  // Space → fit-all
  // Esc → close modal/clear selection
  window.addEventListener('keydown', (e) => {
    const tag = e.target && e.target.tagName;
    const inField = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
    if ((e.metaKey || e.ctrlKey) && (e.key === 'b' || e.key === 'B')) {
      e.preventDefault();
      $container.classList.toggle('sidebar-collapsed');
      onResize();
      return;
    }
    if (inField) return;
    if (e.key === '?') {
      e.preventDefault();
      $cheat.classList.toggle('hidden');
    } else if (e.key === ' ') {
      e.preventDefault();
      fitAll(boot);
    } else if (e.key === 'Escape') {
      showModal(null);
      selectedIds = null;
      updateNodeColors(boot);
    }
  });
}

// ---------- 8. Search rows ----------
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
      updateNodeColors(boot);
      return;
    }

    let final;
    if (search !== null && include !== null) {
      final = new Set();
      for (const id of search) if (include.has(id)) final.add(id);
    } else if (search !== null) {
      final = new Set(search);
    } else if (include !== null) {
      final = new Set(include);
    } else {
      final = new Set(boot.ids);
    }
    if (exclude !== null) {
      for (const id of exclude) final.delete(id);
    }

    selectedIds = final;
    updateNodeColors(boot);
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

// ---------- 9. Modal ----------
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
  const boot = await loadBootstrap();
  initThree();
  buildEdgeMesh(boot);
  buildNodeMesh(boot);
  fitAll(boot);
  initialCam.pos.copy(camera.position);
  initialCam.target.copy(controls.target);

  setStats(
    `${boot.nNodes.toLocaleString()} nodes • ` +
    `${Number(boot.init.nEdges).toLocaleString()} edges • ` +
    `${boot.init.numCommunities} communities`
  );

  wireSidebar(boot);
  wireSearch(boot);
  wireRaycast(boot);
  animate();
}

main().catch((e) => {
  console.error(e);
  setStats(`error: ${e.message || e}`);
});
