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

in {
  inherit emptyGraph addNode addEdge removeNode removeEdge;
  inherit getNodeIds getEdgeIds nodeCount edgeCount;
  inherit hasNode hasEdge getNode getEdge;
  inherit neighbors inNeighbors degree edgesFrom edgesTo;
  inherit merge isEmpty isSubgraphOf;
  inherit fromEdgeList toNodeList toEdgeList toGraphJSON;
}
