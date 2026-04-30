// graph-renderer / main.js
//
// Talks to graph-api over HTTP. Wire format split:
//   - bulk numeric (positions, edges, metrics) → raw Float32Array / Uint32Array
//   - structured (init, node metadata, search) → protobuf via protobufjs
//   - id list (/graph/ids) → JSON, fetched once at startup
//
// Future: when graph-api lives on a remote host, only fetch URLs change.

import { Graph } from 'https://esm.sh/@cosmograph/cosmos@1.3.0';
import protobuf from 'https://esm.sh/protobufjs@7';

const $stats  = document.getElementById('stats');
const $search = document.getElementById('search');
const $modal  = document.getElementById('modal');
const $canvas = document.getElementById('cosmos');

let Init = null, NodeMeta = null, SearchResults = null;

async function loadProto() {
  const root = await protobuf.load('/assets/proto/graph.proto');
  Init          = root.lookupType('jumpcannon.graph.Init');
  NodeMeta      = root.lookupType('jumpcannon.graph.NodeMeta');
  SearchResults = root.lookupType('jumpcannon.graph.SearchResults');
}

async function fetchProto(url, type) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} -> ${r.status}`);
  const buf = new Uint8Array(await r.arrayBuffer());
  return type.decode(buf);
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

function setStats(text) {
  $stats.textContent = text;
}

function showModal(meta) {
  if (!meta) {
    $modal.classList.add('hidden');
    return;
  }
  const tags = (meta.tags || []).map(t => `#${t}`).join(' ');
  let frontmatter = {};
  try { frontmatter = JSON.parse(meta.frontmatterJson || '{}'); } catch (_) {}
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

async function main() {
  setStats('loading schema…');
  await loadProto();

  setStats('loading graph…');
  // /graph/ids returns JSON [id0, id1, ...] in dense-index order so the
  // renderer can use the server's string ids (vault paths) directly with
  // Cosmograph and pass them back to /node/:id without a translation step.
  const init      = await fetchProto('/graph/init', Init);
  const ids       = await fetchJson('/graph/ids');
  const positions = await fetchF32('/graph/positions');
  const edges     = await fetchU32('/graph/edges');
  const community = await fetchF32('/graph/metrics/community');

  // Init.palette is flat [r,g,b,r,g,b,...].
  const palette = [];
  for (let i = 0; i + 2 < init.palette.length; i += 3) {
    palette.push([init.palette[i], init.palette[i + 1], init.palette[i + 2]]);
  }

  const nNodes = Number(init.nNodes);
  const nodes = new Array(nNodes);
  for (let i = 0; i < nNodes; i++) {
    nodes[i] = {
      id: ids[i],
      x: positions[i * 2],
      y: positions[i * 2 + 1],
      community: community[i] | 0,
    };
  }
  const links = new Array(edges.length / 2);
  for (let i = 0; i < links.length; i++) {
    links[i] = { source: ids[edges[i * 2]], target: ids[edges[i * 2 + 1]] };
  }

  const colorFor = (n) => {
    const c = palette[n.community % Math.max(palette.length, 1)];
    return c ? [c[0], c[1], c[2], 1.0] : [0.7, 0.7, 0.7, 1.0];
  };

  const graph = new Graph($canvas, {
    spaceSize: 8192,
    backgroundColor: '#0d0d10',
    nodeSize: 4,
    nodeColor: colorFor,
    linkColor: [0.3, 0.3, 0.35, 0.4],
    linkWidth: 1,
    simulation: {
      friction: 0.85,
      linkDistance: 8,
      gravity: 0.25,
      repulsion: 0.5,
      decay: 1000,
    },
    events: {
      onClick: async (node) => {
        if (!node) { showModal(null); return; }
        try {
          const meta = await fetchProto(`/node/${encodeURIComponent(node.id)}`, NodeMeta);
          showModal(meta);
        } catch (e) {
          console.error(e);
        }
      },
    },
  });

  graph.setData(nodes, links);
  setStats(`${nNodes.toLocaleString()} nodes • ${Number(init.nEdges).toLocaleString()} edges • ${init.numCommunities} communities`);

  // Search → /search?q=… → highlight matches
  let searchTimer = null;
  $search.addEventListener('input', () => {
    clearTimeout(searchTimer);
    const q = $search.value.trim();
    searchTimer = setTimeout(async () => {
      if (!q) { graph.unselectNodes?.(); return; }
      try {
        const r = await fetchProto(`/search?q=${encodeURIComponent(q)}`, SearchResults);
        if (graph.selectNodesByIds) graph.selectNodesByIds(r.ids);
      } catch (e) { console.error(e); }
    }, 150);
  });
}

main().catch((e) => {
  console.error(e);
  setStats(`error: ${e.message || e}`);
});
