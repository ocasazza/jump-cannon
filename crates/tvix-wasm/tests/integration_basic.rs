//! Integration coverage for the public `eval_graph` entry point: a real Nix
//! expression, evaluated against the embedded graph library, yields a typed
//! graph with the expected node/edge counts.

#[test]
fn integration_eval_star_graph() {
    let expr = r#"
        let
          g  = import /jc/src/graph.nix {};
          gc = import /jc/src/graph-combinators.nix { graph = g; };
        in g.toGraphJSON (gc.starGen { nodes = 6; prefix = "n"; })
    "#;

    let graph = tvix_wasm::eval_graph(expr).expect("star graph eval should succeed");
    // center + 5 spokes, 5 hub->spoke edges.
    assert_eq!(graph.nodes.len(), 6);
    assert_eq!(graph.edges.len(), 5);
    assert!(graph.edges.iter().all(|e| e.source == "n0"));
}

#[test]
fn integration_path_graph() {
    let expr = r#"
        let
          g  = import /jc/src/graph.nix {};
          gc = import /jc/src/graph-combinators.nix { graph = g; };
        in g.toGraphJSON (gc.pathGen { nodes = 4; prefix = "p"; })
    "#;

    let graph = tvix_wasm::eval_graph(expr).expect("path graph eval should succeed");
    assert_eq!(graph.nodes.len(), 4);
    assert_eq!(graph.edges.len(), 3);
}
