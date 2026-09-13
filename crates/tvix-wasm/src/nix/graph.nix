# graph.nix — Graph data structures and operations for bird-nix
# Dogfoods birds.nix — combinators imported, not redefined
#
# Graphs use attrsets for O(1) lookup:
#   { nodes = { <id> = { id, type, metadata }; ... };
#     edges = { <id> = { id, source, target, directed, metadata }; ... }; }

{ }:

let
  birds = import ./birds.nix {};
  inherit (birds) I K B;

  # ── Data structures ─────────────────────────────────────────────

  emptyGraph = { nodes = {}; edges = {}; };

  # ── Core operations ─────────────────────────────────────────────

  # addNode : nodeId -> nodeType -> graph -> graph
  addNode = nodeId: nodeType: graph:
    graph // {
      nodes = graph.nodes // {
        ${nodeId} = { id = nodeId; type = nodeType; metadata = {}; };
      };
    };

  # addEdge : edgeId -> source -> target -> directed -> graph -> graph
  addEdge = edgeId: source: target: directed: graph:
    graph // {
      edges = graph.edges // {
        ${edgeId} = { id = edgeId; inherit source target directed; metadata = {}; };
      };
    };

  # removeNode : nodeId -> graph -> graph
  # Also removes all edges connected to that node
  removeNode = nodeId: graph:
    let
      newNodes = builtins.removeAttrs graph.nodes [ nodeId ];
      newEdges = builtins.listToAttrs (
        builtins.filter (e: e.value.source != nodeId && e.value.target != nodeId)
          (builtins.map (eid: { name = eid; value = graph.edges.${eid}; })
            (builtins.attrNames graph.edges))
      );
    in { nodes = newNodes; edges = newEdges; };

  # removeEdge : edgeId -> graph -> graph
  removeEdge = edgeId: graph:
    graph // { edges = builtins.removeAttrs graph.edges [ edgeId ]; };

  # ── Queries ─────────────────────────────────────────────────────

  getNodeIds = graph: builtins.attrNames graph.nodes;
  getEdgeIds = graph: builtins.attrNames graph.edges;

  # NOTE: point-free B builtins.length getNodeIds breaks in tvix-eval because
  # closures capturing builtins across import boundaries lose their context.
  # Use explicit lambdas as a workaround.
  nodeCount = graph: builtins.length (getNodeIds graph);
  edgeCount = graph: builtins.length (getEdgeIds graph);

  hasNode = nodeId: graph: graph.nodes ? ${nodeId};
  hasEdge = edgeId: graph: graph.edges ? ${edgeId};

  getNode = nodeId: graph: graph.nodes.${nodeId};
  getEdge = edgeId: graph: graph.edges.${edgeId};

  # ── Graph queries ───────────────────────────────────────────────

  # neighbors : nodeId -> graph -> [nodeId]  (outgoing)
  neighbors = nodeId: graph:
    builtins.map (e: e.target) (edgesFrom nodeId graph);

  # inNeighbors : nodeId -> graph -> [nodeId]  (incoming)
  inNeighbors = nodeId: graph:
    builtins.map (e: e.source) (edgesTo nodeId graph);

  # degree : nodeId -> graph -> int  (out-degree)
  degree = nodeId: graph:
    builtins.length (edgesFrom nodeId graph);

  # edgesFrom : nodeId -> graph -> [edge]  (outgoing edge records)
  edgesFrom = nodeId: graph:
    builtins.filter (e: e.source == nodeId)
      (builtins.attrValues graph.edges);

  # edgesTo : nodeId -> graph -> [edge]  (incoming edge records)
  edgesTo = nodeId: graph:
    builtins.filter (e: e.target == nodeId)
      (builtins.attrValues graph.edges);

  # ── Graph merge ─────────────────────────────────────────────────

  # merge : graph -> graph -> graph  (union of nodes and edges)
  merge = g1: g2: {
    nodes = g1.nodes // g2.nodes;
    edges = g1.edges // g2.edges;
  };

  # ── Predicates ──────────────────────────────────────────────────

  # isEmpty : graph -> bool
  isEmpty = graph: (nodeCount graph == 0) && (edgeCount graph == 0);

  # isSubgraphOf : g1 -> g2 -> bool  (all nodes/edges of g1 exist in g2)
  isSubgraphOf = g1: g2:
    let
      nodesOk = builtins.all (nid: g2.nodes ? ${nid}) (getNodeIds g1);
      edgesOk = builtins.all (eid: g2.edges ? ${eid}) (getEdgeIds g1);
    in nodesOk && edgesOk;

  # ── Convenience ─────────────────────────────────────────────────

  # fromEdgeList : [{ source, target }] -> graph
  # Builds a graph from a list of {source, target} attrsets.
  # Auto-generates edge IDs and adds nodes for each endpoint.
  fromEdgeList = edgeSpecs:
    let
      addOne = acc: spec:
        let
          idx = builtins.toString acc.idx;
          src = spec.source;
          tgt = spec.target;
          directed = spec.directed or true;
          g1 = addNode src (spec.sourceType or "node") acc.graph;
          g2 = addNode tgt (spec.targetType or "node") g1;
          g3 = addEdge ("e" + idx) src tgt directed g2;
        in { graph = g3; idx = acc.idx + 1; };
      result = builtins.foldl' addOne { graph = emptyGraph; idx = 0; } edgeSpecs;
    in result.graph;

  # toNodeList : graph -> [node]
  toNodeList = graph: builtins.attrValues graph.nodes;

  # toEdgeList : graph -> [edge]
  toEdgeList = graph: builtins.attrValues graph.edges;

  # toGraphJSON : graph -> { nodes : [{ id, type, ... }], links : [{ source, target, id, ... }] }
  # Converts internal graph representation to force-graph compatible JSON structure.
  # Nodes get { id, type } (plus any metadata fields).
  # Links get { source, target, id, directed } (plus any metadata fields).
  toGraphJSON = graph:
    let
      nodes = builtins.map (n: { inherit (n) id type; } // n.metadata) (toNodeList graph);
      links = builtins.map (e: {
        inherit (e) source target id directed;
      } // e.metadata) (toEdgeList graph);
    in { inherit nodes links; };

  # ── Random generators ───────────────────────────────────────────
  #
  # Pure, deterministic pseudo-random graph generators. They replace the
  # retired direct-Rust GenerateLoader: an `engine = "tvix"` importer package
  # binds one of these, and the host supplies `nodes`, `edges`, `seed` (and,
  # for `clustered`, `clusters` and `affinity`) at apply time. The RNG is a
  # per-index LCG (Numerical Recipes constants) reduced modulo 2^32, so a given
  # parameter tuple always yields the identical graph with no impure builtins.
  # Nodes and edges are each built in one `listToAttrs` (O(n+e)), avoiding the
  # O(n^2) successive-merge cost of repeated addNode/addEdge.

  rngModulus = 4294967296; # 2^32
  rngReduce = x: x - (rngModulus * (x / rngModulus)); # x mod 2^32, x >= 0
  rngStep = x: rngReduce (1664525 * x + 1013904223);
  rngMix = x: rngStep (rngStep (rngReduce x));
  # rand : seed -> index -> stream -> int in [0, 2^32). Distinct streams give
  # independent draws for the same edge index (source, target, coin flip).
  rand = seed: i: stream: rngMix ((rngReduce seed) + i * 2654435761 + stream * 40503);

  # random : { nodes, edges, seed } -> toGraphJSON graph
  # `nodes` isolated nodes n0..n{nodes-1}, then `edges` directed edges wired
  # uniformly at random between them (no self-loops). Every node carries the
  # `generated` type, surfaced as a tag by the importer projection.
  random = { nodes ? 1000, edges ? 2000, seed ? 0 }:
    let
      n = nodes;
      modn = a: a - (n * (a / n));
      nodeEntries = builtins.genList (i:
        let id = "n" + builtins.toString i; in {
          name = id;
          value = { id = id; type = "generated"; metadata = { }; };
        }) n;
      edgeEntries =
        if n == 0 then [ ]
        else builtins.genList (i:
          let
            src = modn (rand seed i 1);
            t0 = modn (rand seed i 2);
            tgt = if n > 1 && t0 == src then modn (t0 + 1) else t0;
            eid = "e" + builtins.toString i;
          in {
            name = eid;
            value = {
              id = eid;
              source = "n" + builtins.toString src;
              target = "n" + builtins.toString tgt;
              directed = true;
              metadata = { };
            };
          }) edges;
    in
    toGraphJSON {
      nodes = builtins.listToAttrs nodeEntries;
      edges = builtins.listToAttrs edgeEntries;
    };

  # clustered : { nodes, edges, clusters, affinity, seed } -> toGraphJSON graph
  # Round-robin partitions the nodes into `clusters` communities (node i in
  # cluster i mod clusters). With probability `affinity` an edge connects two
  # nodes of the same cluster; otherwise it connects two random nodes. Each
  # node's type is `cluster-<c>`, so the importer tag projection — and thus the
  # facet strip — exposes the community structure. `clusters = 0` degrades to
  # the uniform `random` topology.
  clustered = { nodes ? 1000, edges ? 2000, clusters ? 8, affinity ? 0.8, seed ? 0 }:
    let
      n = nodes;
      k = if clusters > 0 && n > 0 then (if clusters < n then clusters else n) else 0;
      modn = a: a - (n * (a / n));
      clusterOf = i: i - (k * (i / k)); # i mod k; only evaluated when k > 0
      nodeEntries = builtins.genList (i:
        let
          id = "n" + builtins.toString i;
          ty = if k > 0 then "cluster-" + builtins.toString (clusterOf i) else "generated";
        in {
          name = id;
          value = { id = id; type = ty; metadata = { }; };
        }) n;
      edgeEntries =
        if n == 0 then [ ]
        else builtins.genList (i:
          let
            src = modn (rand seed i 1);
            rt = rand seed i 2;
            roll = (1.0 * (let r = rand seed i 3; in r - (1000000 * (r / 1000000)))) / 1000000.0;
            c = clusterOf src;
            # Cluster c holds nodes c, c+k, c+2k, … below n; cnt of them.
            cnt = if k > 0 then ((n - c) + k - 1) / k else 0;
            intra = k > 0 && cnt > 1 && roll < affinity;
            j0 = rt - (cnt * (rt / cnt)); # rt mod cnt; only used when cnt > 1
            tIntra =
              let t = c + j0 * k; in
              if t == src then c + (if j0 + 1 < cnt then j0 + 1 else 0) * k else t;
            t0 = modn rt;
            tInter = if n > 1 && t0 == src then modn (t0 + 1) else t0;
            tgt = if intra then tIntra else tInter;
            eid = "e" + builtins.toString i;
          in {
            name = eid;
            value = {
              id = eid;
              source = "n" + builtins.toString src;
              target = "n" + builtins.toString tgt;
              directed = true;
              metadata = { };
            };
          }) edges;
    in
    toGraphJSON {
      nodes = builtins.listToAttrs nodeEntries;
      edges = builtins.listToAttrs edgeEntries;
    };

in {
  inherit emptyGraph addNode addEdge removeNode removeEdge;
  inherit getNodeIds getEdgeIds nodeCount edgeCount;
  inherit hasNode hasEdge getNode getEdge;
  inherit neighbors inNeighbors degree edgesFrom edgesTo;
  inherit merge isEmpty isSubgraphOf;
  inherit fromEdgeList toNodeList toEdgeList toGraphJSON;
  inherit random clustered;
}
