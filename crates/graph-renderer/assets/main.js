// graph-renderer / main.js
//
// Talks to graph-api over HTTP. Wire format split:
//   - bulk numeric (positions, edges, metrics) → raw Float32Array / Uint32Array
//   - structured (init, node metadata, search) → protobuf via protobufjs
//   - id list (/graph/ids) → JSON, fetched once at startup

import { Graph } from 'https://esm.sh/@cosmograph/cosmos@1.3.0';
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
function gradient(t) {
  // blue → red sequential gradient. t in [0,1].
  t = Math.max(0, Math.min(1, t));
  const r = 0.2 + t * (0.95 - 0.2);
  const g = 0.4 + t * (0.2  - 0.4);
  const b = 0.95 + t * (0.25 - 0.95);
  return [r, g, b, 1.0];
}

function paletteColor(palette, idx) {
  const c = palette[((idx | 0) % Math.max(palette.length, 1) + palette.length) % Math.max(palette.length, 1)];
  return c ? [c[0], c[1], c[2], 1.0] : [0.7, 0.7, 0.7, 1.0];
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
  const nodes = new Array(nNodes);
  for (let i = 0; i < nNodes; i++) {
    idToIdx.set(ids[i], i);
    nodes[i] = { id: ids[i], x: positions[i * 2], y: positions[i * 2 + 1] };
  }
  const links = new Array(edges.length / 2);
  for (let i = 0; i < links.length; i++) {
    links[i] = { source: ids[edges[i * 2]], target: ids[edges[i * 2 + 1]] };
  }

  return { init, ids, idToIdx, nodes, links, palette, metrics, bounds, nNodes };
}

// ---------- 2. Build graph ----------
const SIM_PRESETS = {
  fast:     { friction: 0.85, decay: 300,  repulsion: 1.0, gravity: 0.1, linkSpring: 0.4, linkDistance: 8 },
  balanced: { friction: 0.95, decay: 5000, repulsion: 1.5, gravity: 0.1, linkSpring: 0.4, linkDistance: 8 },
  pretty:   { friction: 0.95, decay: 2000, repulsion: 2.0, gravity: 0.1, linkSpring: 0.6, linkDistance: 12 },
};

const state = {
  sizeBy: 'degree',
  sizeMul: 1.0,
  colorBy: 'community',
  preset: 'balanced',
};

function makeSizeAccessor(boot) {
  const arr = boot.metrics[state.sizeBy];
  const { min, max } = boot.bounds[state.sizeBy];
  const span = max - min;
  const mul = state.sizeMul;
  // Identity must change each call so Cosmograph's setConfig diff fires.
  return (n) => {
    const idx = boot.idToIdx.get(n.id);
    if (idx === undefined) return 2 * mul;
    if (span <= 0) return 2 * mul;
    const t = Math.sqrt((arr[idx] - min) / span);
    return (1 + t * 9) * mul;
  };
}

function makeColorAccessor(boot) {
  const key = state.colorBy;
  if (key === 'community' || key === 'wcc') {
    const arr = boot.metrics[key];
    return (n) => {
      const idx = boot.idToIdx.get(n.id);
      return paletteColor(boot.palette, idx === undefined ? 0 : (arr[idx] | 0));
    };
  }
  if (key === 'folder') {
    // TODO: fetch per-node folder via NodeMeta or a bulk endpoint;
    // currently fall back to community-by-palette.
    const arr = boot.metrics.community;
    return (n) => {
      const idx = boot.idToIdx.get(n.id);
      return paletteColor(boot.palette, idx === undefined ? 0 : (arr[idx] | 0));
    };
  }
  // Sequential: indegree, kcore.
  const arr = boot.metrics[key];
  const { min, max } = boot.bounds[key];
  const span = max - min;
  return (n) => {
    const idx = boot.idToIdx.get(n.id);
    if (idx === undefined || span <= 0) return gradient(0);
    return gradient((arr[idx] - min) / span);
  };
}

function refreshAccessors(graph, boot) {
  // Always mint NEW function references; Cosmograph skips identical refs.
  graph.setConfig({
    nodeSize: makeSizeAccessor(boot),
    nodeColor: makeColorAccessor(boot),
  });
}

function buildGraph(boot) {
  const graph = new Graph($canvas, {
    spaceSize: 8192,
    backgroundColor: '#0d0d10',
    nodeSize: makeSizeAccessor(boot),
    nodeColor: makeColorAccessor(boot),
    linkColor: [0.3, 0.3, 0.35, 0.4],
    linkWidth: 1,
    simulation: { ...SIM_PRESETS[state.preset] },
    events: {
      onClick: async (node) => {
        if (!node) { showModal(null); return; }
        try {
          const meta = await fetchProto(`/node/${encodeURIComponent(node.id)}`, NodeMeta);
          showModal(meta);
        } catch (e) { console.error(e); }
      },
    },
  });
  graph.setData(boot.nodes, boot.links);
  return graph;
}

// ---------- 3. Sidebar controls ----------
function wireSidebar(graph, boot) {
  $sizeBy.addEventListener('change', () => {
    state.sizeBy = $sizeBy.value;
    refreshAccessors(graph, boot);
  });
  $sizeMul.addEventListener('input', () => {
    state.sizeMul = parseFloat($sizeMul.value);
    $sizeMulV.textContent = state.sizeMul.toFixed(2);
    refreshAccessors(graph, boot);
  });
  $colorBy.addEventListener('change', () => {
    state.colorBy = $colorBy.value;
    refreshAccessors(graph, boot);
  });

  $simBtns.forEach((btn) => {
    btn.addEventListener('click', () => {
      const preset = btn.dataset.preset;
      if (!SIM_PRESETS[preset]) return;
      state.preset = preset;
      $simBtns.forEach((b) => b.classList.toggle('active', b === btn));
      graph.setConfig({ simulation: { ...SIM_PRESETS[preset] } });
      if (typeof graph.start === 'function') graph.start();
    });
  });

  $camFit.addEventListener('click', () => {
    if (typeof graph.fitView === 'function') graph.fitView(500);
  });
  $camReset.addEventListener('click', () => {
    if (typeof graph.zoom === 'function')           graph.zoom(1.0, 500);
    else if (typeof graph.setZoomLevel === 'function') graph.setZoomLevel(1.0);
  });

  // Cmd/Ctrl + B → toggle sidebar
  window.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && (e.key === 'b' || e.key === 'B')) {
      e.preventDefault();
      $container.classList.toggle('sidebar-collapsed');
    }
  });
}

// ---------- 4. Search rows ----------
function wireSearch(graph, boot) {
  let regexMode = false;
  const lastSets = { search: null, include: null, exclude: null }; // null = "no query"

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

    // If ALL rows empty → unselect.
    if (search === null && include === null && exclude === null) {
      if (graph.unselectNodes) graph.unselectNodes();
      return;
    }

    let final;
    // search ∩ include
    if (search !== null && include !== null) {
      final = new Set();
      for (const id of search) if (include.has(id)) final.add(id);
    } else if (search !== null) {
      final = new Set(search);
    } else if (include !== null) {
      final = new Set(include);
    } else {
      final = new Set(boot.ids); // both empty, but exclude is set
    }
    // \ exclude
    if (exclude !== null) {
      for (const id of exclude) final.delete(id);
    }

    if (final.size === 0) {
      // Sentinel: select an id that doesn't exist → highlights nothing.
      if (graph.selectNodesByIds) graph.selectNodesByIds(['__nope__:no-match']);
      return;
    }
    if (graph.selectNodesByIds) graph.selectNodesByIds(Array.from(final));
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
    // Re-run search row immediately with new mode.
    $search.dispatchEvent(new Event('input'));
  });
}

// ---------- 5. Modal ----------
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

function wireModal() {
  // Click-away to dismiss.
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') showModal(null);
  });
}

// ---------- main ----------
async function main() {
  const boot = await loadBootstrap();
  const graph = buildGraph(boot);
  setStats(
    `${boot.nNodes.toLocaleString()} nodes • ` +
    `${Number(boot.init.nEdges).toLocaleString()} edges • ` +
    `${boot.init.numCommunities} communities`
  );
  wireSidebar(graph, boot);
  wireSearch(graph, boot);
  wireModal();
}

main().catch((e) => {
  console.error(e);
  setStats(`error: ${e.message || e}`);
});
