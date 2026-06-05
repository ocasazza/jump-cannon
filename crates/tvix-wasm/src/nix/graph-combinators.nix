# graph-combinators.nix — Combinator-powered graph transformations
#
# IMPORTANT: tvix-eval has a bug where closures captured across 3+ levels
# of file imports break. To work around this, graph-combinators accepts
# graph operations as a parameter instead of importing graph.nix directly.
#
# Usage:
#   let g = import ./graph.nix {};
#       gcl = import ./graph-combinators.nix { graph = g; };
#   in gcl.pathGen { nodes = 5; prefix = "n"; }

{ graph }:

let
  inherit (graph) emptyGraph addNode addEdge removeNode removeEdge
    getNodeIds getEdgeIds nodeCount edgeCount
    hasNode hasEdge getNode getEdge
    neighbors inNeighbors degree edgesFrom edgesTo
    merge isEmpty;

  # ── Helpers ──────────────────────────────────────────────────────

  str = builtins.toString;

  # Y combinator — inlined to avoid importing birds.nix (tvix closure bug)
  Y = f: let x = f x; in x;

  # ── Graph Generators ───────────────────────────────────────────

  # pathGen : { nodes, prefix } -> graph
  # Linear chain: prefix0 -> prefix1 -> prefix2 -> ...
  pathGen = Y (self: params:
    let
      n = params.nodes or 5;
      p = params.prefix or "n";
    in
    if n <= 0 then emptyGraph
    else if n == 1 then addNode (p + "0") "node" emptyGraph
    else
      let
        prev = self { nodes = n - 1; prefix = p; };
        last = p + str (n - 1);
        secondLast = p + str (n - 2);
      in
        addEdge (secondLast + "->" + last) secondLast last true
          (addNode last "node" prev));

  # starGen : { nodes, prefix } -> graph
  # Hub-and-spoke: center -> spoke1, center -> spoke2, ...
  starGen = Y (self: params:
    let
      n = params.nodes or 5;
      p = params.prefix or "n";
      center = p + "0";
    in
    if n <= 0 then emptyGraph
    else if n == 1 then addNode center "center" emptyGraph
    else
      let
        prev = self { nodes = n - 1; prefix = p; };
        spoke = p + str (n - 1);
      in
        addEdge (center + "->" + spoke) center spoke true
          (addNode spoke "spoke" prev));

  # completeGen : { nodes, prefix } -> graph
  # Fully connected: every node connected to every other node
  completeGen = params:
    let
      n = params.nodes or 5;
      p = params.prefix or "n";
      ids = builtins.genList (i: p + str i) n;
      withNodes = builtins.foldl' (g: nid: addNode nid "node" g) emptyGraph ids;
      pairs = builtins.concatMap (i:
        builtins.concatMap (j:
          let
            src = builtins.elemAt ids i;
            tgt = builtins.elemAt ids j;
          in
          if i != j then [{ source = src; target = tgt; eid = src + "->" + tgt; }]
          else []
        ) (builtins.genList (x: x) n)
      ) (builtins.genList (x: x) n);
    in
    builtins.foldl' (g: pair:
      addEdge pair.eid pair.source pair.target true g
    ) withNodes pairs;

  # cycleGen : { nodes, prefix } -> graph
  # Like path but last node connects back to first
  cycleGen = params:
    let
      n = params.nodes or 5;
      p = params.prefix or "n";
      path = pathGen { nodes = n; prefix = p; };
      last = p + str (n - 1);
      first = p + "0";
    in
    if n <= 1 then path
    else addEdge (last + "->" + first) last first true path;

  # gridGen : { rows, cols, prefix } -> graph
  # 2D lattice: each node connects right and down
  gridGen = params:
    let
      rows = params.rows or 3;
      cols = params.cols or 3;
      p = params.prefix or "n";
      nid = r: c: p + str r + "_" + str c;
      # Create all nodes
      withNodes = builtins.foldl' (g: r:
        builtins.foldl' (g2: c:
          addNode (nid r c) "node" g2
        ) g (builtins.genList (x: x) cols)
      ) emptyGraph (builtins.genList (x: x) rows);
      # Horizontal edges (right)
      hEdges = builtins.concatMap (r:
        builtins.concatMap (c:
          if c + 1 < cols
          then [{ src = nid r c; tgt = nid r (c + 1); }]
          else []
        ) (builtins.genList (x: x) cols)
      ) (builtins.genList (x: x) rows);
      # Vertical edges (down)
      vEdges = builtins.concatMap (r:
        builtins.concatMap (c:
          if r + 1 < rows
          then [{ src = nid r c; tgt = nid (r + 1) c; }]
          else []
        ) (builtins.genList (x: x) cols)
      ) (builtins.genList (x: x) rows);
      allEdges = hEdges ++ vEdges;
    in
    builtins.foldl' (g: e:
      addEdge (e.src + "->" + e.tgt) e.src e.tgt true g
    ) withNodes allEdges;

  # ── Graph Rewrite Rules ─────────────────────────────────────────

  # subdivideRule : graph -> edgeId -> graph
  # Replace edge a->b with a->c->b (insert intermediate node)
  subdivideRule = g: edgeId:
    let
      e = getEdge edgeId g;
      mid = e.source + "_" + e.target + "_mid";
      g1 = removeEdge edgeId g;
      g2 = addNode mid "intermediate" g1;
      g3 = addEdge (edgeId + "_a") e.source mid e.directed g2;
    in addEdge (edgeId + "_b") mid e.target e.directed g3;

  # hubRule : graph -> graph
  # Add a hub node connected to all existing nodes (bidirectional)
  hubRule = g:
    let
      hubId = "hub";
      nids = getNodeIds g;
      g1 = addNode hubId "hub" g;
    in
    builtins.foldl' (acc: nid:
      addEdge (nid + "->hub") nid hubId true
        (addEdge ("hub->" + nid) hubId nid true acc)
    ) g1 nids;

  # reverseEdgesRule : graph -> graph
  # Reverse all directed edges
  reverseEdgesRule = g:
    let
      edgeIds = getEdgeIds g;
    in
    builtins.foldl' (acc: eid:
      let
        e = getEdge eid g;
      in
      acc // {
        edges = acc.edges // {
          ${eid} = e // { source = e.target; target = e.source; };
        };
      }
    ) g edgeIds;

  # composeOps : (graph -> graph) -> (graph -> graph) -> graph -> graph
  composeOps = f: g: x: f (g x);

  # contractRule : graph -> edgeId -> graph
  # Contract an edge: merge target into source, redirect edges
  contractRule = gr: edgeId:
    let
      e = getEdge edgeId gr;
      src = e.source;
      tgt = e.target;
      # Redirect edges that point to/from tgt to point to/from src instead
      allEdgeIds = getEdgeIds gr;
      redirected = builtins.foldl' (acc: eid:
        let
          edge = getEdge eid gr;
          newSource = if edge.source == tgt then src else edge.source;
          newTarget = if edge.target == tgt then src else edge.target;
        in
        acc // {
          edges = acc.edges // {
            ${eid} = edge // { source = newSource; target = newTarget; };
          };
        }
      ) gr allEdgeIds;
      # Remove the contracted edge and the target node
      g1 = removeEdge edgeId redirected;
    in removeNode tgt g1;

  # ── Combinator Bird Operations on Graphs ─────────────────────

  # I combinator: identity — graph unchanged
  identityGraph = x: x;

  # K combinator: const graph — ignore input, return a single-node graph
  constGraph = nodeId: nodeType: _: addNode nodeId nodeType emptyGraph;

  # V combinator: vireo — pair two items, apply selector
  edgePair = src: tgt: f: f src tgt;

  # B combinator: composition — already defined as composeOps

  # W combinator: duplicate argument
  dupArg = f: x: f x x;

  # C combinator: flip arguments
  flipOps = f: x: y: f y x;

  # Parallel merge — apply two ops and merge results
  parallelMerge = f: gOp: x: merge (f x) (gOp x);

  # W-based self-loop: add node and loop edge to itself
  makeSelfLoop = nodeId: gr:
    let
      g1 = addNode nodeId "node" gr;
    in addEdge (nodeId + "->" + nodeId) nodeId nodeId true g1;

  # ── Algebraic Identity Demonstrations ────────────────────────

  # S K K = I : the classic combinator identity proof
  graphIdentity =
    let
      S = f: gOp: x: f x (gOp x);
      K = x: _: x;
    in S K K;

  # W K = I : warbler-kestrel identity
  warblerKestrelIdentity =
    let
      W = f: x: f x x;
      K = x: _: x;
    in W K;

  # ── Type Environment ─────────────────────────────────────────

  gclTypeEnv = {
    I = "Graph -> Graph";
    K = "Graph -> Graph -> Graph";
    V = "a -> b -> (a -> b -> c) -> c";
    B = "(b -> c) -> (a -> b) -> a -> c";
    S = "(a -> b -> c) -> (a -> b) -> a -> c";
    W = "(a -> a -> b) -> a -> b";
    M = "(a -> a) -> a";
    Y = "(a -> a) -> a";
  };

  # ── Speech Descriptions ──────────────────────────────────────

  graphSpeech = {
    identityGraph = {
      bird = "I";
      speech = "I am the identity bird. Whatever graph you give me, I give it right back unchanged.";
    };
    edgePair = {
      bird = "V";
      speech = "I am the vireo. I pair a source and target, then let you choose which to extract.";
    };
    pathGen = {
      bird = "path";
      speech = "I generate a linear path graph: n0 -> n1 -> n2 -> ...";
    };
    starGen = {
      bird = "star";
      speech = "I generate a star graph with a center hub connected to all spokes.";
    };
    completeGen = {
      bird = "complete";
      speech = "I generate a complete graph where every node connects to every other.";
    };
    subdivideRule = {
      bird = "subdivide";
      speech = "I take an edge and insert a midpoint, splitting it into two edges.";
    };
    hubRule = {
      bird = "hub";
      speech = "I add a hub node connected bidirectionally to all existing nodes.";
    };
  };

in {
  inherit pathGen starGen completeGen cycleGen gridGen;
  inherit subdivideRule hubRule reverseEdgesRule contractRule composeOps;
  inherit identityGraph constGraph edgePair dupArg flipOps parallelMerge makeSelfLoop;
  inherit graphIdentity warblerKestrelIdentity;
  inherit gclTypeEnv graphSpeech;
}
