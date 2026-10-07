//! Property tests: each metric against an independent reference or invariant.
//!
//! `PROPTEST_CASES` scales the run (`just test fuzz`).

use proptest::prelude::*;
use std::collections::VecDeque;
use vault_data::{VaultEdge, VaultGraph, VaultNode};

fn id(i: usize) -> String {
    format!("n{i}")
}

/// Nodes inserted in index order, or reversed when `reverse` is set.
fn build(n: usize, edges: &[(usize, usize)], reverse: bool) -> VaultGraph {
    let mut g = VaultGraph::new();
    let order: Vec<usize> = if reverse { (0..n).rev().collect() } else { (0..n).collect() };
    for i in order {
        g.add_node(VaultNode { id: id(i), ..Default::default() });
    }
    for &(a, b) in edges {
        g.add_edge(VaultEdge::new(id(a), id(b)));
    }
    g
}

/// Directed edges with self-loops, parallel edges, and both directions.
fn multigraph() -> impl Strategy<Value = (usize, Vec<(usize, usize)>)> {
    (1usize..=24).prop_flat_map(|n| (Just(n), prop::collection::vec((0..n, 0..n), 0..=3 * n)))
}

/// Undirected simple graph: each edge once as `(a, b)` with `a < b`.
fn simple_graph() -> impl Strategy<Value = (usize, Vec<(usize, usize)>)> {
    (2usize..=16).prop_flat_map(|n| {
        prop::collection::btree_set((0..n, 0..n), 0..=2 * n).prop_map(move |pairs| {
            let edges = pairs.into_iter().filter(|(a, b)| a < b).collect();
            (n, edges)
        })
    })
}

/// Neighbour sets of the simple undirected graph underlying `edges`.
fn simple_adjacency(n: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    let mut adj = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a != b {
            adj[a].push(b);
            adj[b].push(a);
        }
    }
    for nbrs in &mut adj {
        nbrs.sort_unstable();
        nbrs.dedup();
    }
    adj
}

/// Core numbers by repeatedly removing a minimum-degree node.
fn reference_core(adj: &[Vec<usize>]) -> Vec<usize> {
    let n = adj.len();
    let mut deg: Vec<usize> = adj.iter().map(Vec::len).collect();
    let mut removed = vec![false; n];
    let mut core = vec![0; n];
    let mut k = 0;
    for _ in 0..n {
        let v = (0..n).filter(|&v| !removed[v]).min_by_key(|&v| deg[v]).unwrap();
        k = k.max(deg[v]);
        core[v] = k;
        removed[v] = true;
        for &w in &adj[v] {
            if !removed[w] {
                deg[w] -= 1;
            }
        }
    }
    core
}

fn same_partition<A: PartialEq, B: PartialEq>(a: &[A], b: &[B]) -> bool {
    (0..a.len()).all(|i| (0..a.len()).all(|j| (a[i] == a[j]) == (b[i] == b[j])))
}

/// Normalized undirected betweenness by enumerating all shortest paths.
fn brute_betweenness(adj: &[Vec<usize>]) -> Vec<f64> {
    let n = adj.len();
    let mut dist = vec![vec![usize::MAX; n]; n];
    let mut paths = vec![vec![0.0f64; n]; n];
    for s in 0..n {
        dist[s][s] = 0;
        paths[s][s] = 1.0;
        let mut queue = VecDeque::from([s]);
        while let Some(v) = queue.pop_front() {
            for &w in &adj[v] {
                if dist[s][w] == usize::MAX {
                    dist[s][w] = dist[s][v] + 1;
                    queue.push_back(w);
                }
                if dist[s][w] == dist[s][v] + 1 {
                    paths[s][w] += paths[s][v];
                }
            }
        }
    }
    let scale = if n > 2 { 2.0 / ((n - 1) as f64 * (n - 2) as f64) } else { 1.0 };
    (0..n)
        .map(|v| {
            let mut total = 0.0;
            for a in 0..n {
                for b in (a + 1)..n {
                    let through = dist[a][v] != usize::MAX
                        && dist[v][b] != usize::MAX
                        && a != v
                        && b != v
                        && dist[a][v] + dist[v][b] == dist[a][b];
                    if through {
                        total += paths[a][v] * paths[v][b] / paths[a][b];
                    }
                }
            }
            total * scale
        })
        .collect()
}

fn modularity(n: usize, edges: &[(usize, usize)], community: &[u32]) -> f64 {
    let m = edges.len() as f64;
    let mut degree = vec![0.0; n];
    let mut inside = 0.0;
    for &(a, b) in edges {
        degree[a] += 1.0;
        degree[b] += 1.0;
        if community[a] == community[b] {
            inside += 1.0;
        }
    }
    let k = community.iter().copied().max().unwrap_or(0) as usize + 1;
    let mut total = vec![0.0; k];
    for i in 0..n {
        total[community[i] as usize] += degree[i];
    }
    inside / m - total.iter().map(|t| (t / (2.0 * m)).powi(2)).sum::<f64>()
}

proptest! {
    #[test]
    fn kcore_matches_peeling_of_simple_graph((n, edges) in multigraph()) {
        let mut g = build(n, &edges, false);
        graph_metrics::compute_kcore(&mut g);
        let got: Vec<usize> = (0..n).map(|i| g.nodes[&id(i)].metrics.kcore).collect();
        prop_assert_eq!(got, reference_core(&simple_adjacency(n, &edges)));
    }

    #[test]
    fn wcc_matches_reachability((n, edges) in multigraph()) {
        let mut g = build(n, &edges, false);
        graph_metrics::compute_wcc(&mut g);
        let got: Vec<usize> = (0..n).map(|i| g.nodes[&id(i)].metrics.wcc).collect();

        let adj = simple_adjacency(n, &edges);
        let mut label = vec![usize::MAX; n];
        let mut components = 0;
        for s in 0..n {
            if label[s] != usize::MAX {
                continue;
            }
            label[s] = components;
            let mut stack = vec![s];
            while let Some(v) = stack.pop() {
                for &w in &adj[v] {
                    if label[w] == usize::MAX {
                        label[w] = components;
                        stack.push(w);
                    }
                }
            }
            components += 1;
        }
        prop_assert!(same_partition(&got, &label));
        prop_assert_eq!(g.num_wcc, components);
    }

    #[test]
    fn exact_betweenness_matches_path_enumeration((n, edges) in simple_graph()) {
        let mut g = build(n, &edges, false);
        graph_metrics::compute_betweenness(&mut g, n);
        let want = brute_betweenness(&simple_adjacency(n, &edges));
        for (i, expected) in want.iter().enumerate() {
            let got = g.nodes[&id(i)].metrics.betweenness;
            prop_assert!((got - expected).abs() < 1e-9, "node {i}: {got} != {expected}");
        }
    }

    #[test]
    fn pagerank_is_a_distribution((n, edges) in multigraph()) {
        let mut g = build(n, &edges, false);
        graph_metrics::compute_pagerank(&mut g, 0.85, 50);
        let total: f64 = g.nodes.values().map(|node| node.metrics.pagerank).sum();
        prop_assert!((total - 1.0).abs() < 1e-9, "sum {total}");
        prop_assert!(g.nodes.values().all(|node| node.metrics.pagerank >= 0.0));
    }

    #[test]
    fn pagerank_ignores_insertion_order((n, edges) in multigraph()) {
        let (mut forward, mut reverse) = (build(n, &edges, false), build(n, &edges, true));
        graph_metrics::compute_pagerank(&mut forward, 0.85, 50);
        graph_metrics::compute_pagerank(&mut reverse, 0.85, 50);
        for i in 0..n {
            let (a, b) = (forward.nodes[&id(i)].metrics.pagerank, reverse.nodes[&id(i)].metrics.pagerank);
            prop_assert!((a - b).abs() < 1e-12, "node {i}: {a} != {b}");
        }
    }

    #[test]
    fn louvain_levels_coarsen_finer_levels((n, edges) in simple_graph()) {
        let mut g = build(n, &edges, false);
        graph_metrics::compute_louvain(&mut g, 10);
        let levels = &g.community_levels;
        for pair in levels.windows(2) {
            let (coarse, fine) = (&pair[0], &pair[1]);
            for a in 0..n {
                for b in 0..n {
                    prop_assert!(fine[a] != fine[b] || coarse[a] == coarse[b]);
                }
            }
        }
        let max = levels[0].iter().copied().max().map_or(0, |m| m as usize + 1);
        prop_assert_eq!(g.num_communities, max);
    }

    #[test]
    fn louvain_does_not_lose_modularity((n, edges) in simple_graph()) {
        prop_assume!(!edges.is_empty());
        let mut g = build(n, &edges, false);
        graph_metrics::compute_louvain(&mut g, 10);
        let singletons: Vec<u32> = (0..n as u32).collect();
        prop_assert!(modularity(n, &edges, &g.community_levels[0]) + 1e-12 >= modularity(n, &edges, &singletons));
    }

    #[test]
    fn louvain_is_deterministic((n, edges) in simple_graph()) {
        let (mut a, mut b) = (build(n, &edges, false), build(n, &edges, false));
        graph_metrics::compute_louvain(&mut a, 10);
        graph_metrics::compute_louvain(&mut b, 10);
        prop_assert_eq!(&a.community_levels, &b.community_levels);
    }
}
