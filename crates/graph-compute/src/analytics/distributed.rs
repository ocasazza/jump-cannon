//! Distributed PageRank by domain decomposition + BSP halo exchange — the
//! scale-out path for graphs too large for one GPU's memory (the 8M+ node,
//! chemical-sim regime).
//!
//! This is the analytics counterpart to [`crate::partition::run_superstep_local`]
//! (which halo-exchanges *positions* for layout): here each partition holds an
//! owned+ghost slice of the CSR (via [`crate::partition::partition_csr`]) and
//! each superstep is
//!   1. **compute** — a local pull-SpMV step over owned rows (the same gather as
//!      [`super::gpu_pagerank`], reading current owned+ghost ranks), then
//!   2. **barrier + halo exchange** — refresh every ghost from the owning
//!      partition's freshly-computed boundary rank.
//!
//! With synchronous BSP this is *identical* to the single-process Jacobi
//! iteration (each owned row's gather sees the same neighbor ranks), so the
//! gathered result matches [`super::cpu_pagerank`] to f32 summation order — the
//! correctness contract the test pins. This in-process version uses a direct
//! scalar exchange; the multi-process path routes the same boundary values
//! through `TonicHaloTransport` (gRPC), and the per-partition compute can run on
//! a per-worker GPU (`gpu_spmv`) for true multi-GPU scale-out — both build on
//! this same owned/ghost/boundary partition structure.
//!
//! Like [`super::gpu_pagerank`], the current version assumes **no dangling
//! (degree-0) nodes**; dangling-mass redistribution needs a global reduction
//! across partitions and is a follow-up.

use crate::analytics::{gpu_spmv, WeightedCsr};
use crate::engines::EngineCtx;
use crate::partition::partition_csr;
use crate::sim::CsrGraph;

/// Distributed PageRank over `n_partitions` in-process partitions. Returns the
/// per-node rank in global node order. Matches [`super::cpu_pagerank`] on graphs
/// with no dangling nodes (to f32 tolerance).
pub fn distributed_pagerank(
    graph: &CsrGraph,
    n_partitions: u32,
    damping: f32,
    iters: u32,
) -> Vec<f32> {
    let n_global = graph.n_nodes as usize;
    if n_global == 0 {
        return Vec::new();
    }

    let parts = partition_csr(graph, None, n_partitions);

    // Global inverse degree (each node's full-graph degree — a partition's owned
    // rows carry all global neighbors, so the gather is complete locally).
    let inv_deg_global: Vec<f32> = (0..n_global)
        .map(|v| {
            let d = graph.offsets[v + 1] - graph.offsets[v];
            if d == 0 {
                0.0
            } else {
                1.0 / d as f32
            }
        })
        .collect();

    let inv_n = 1.0 / n_global as f32;
    let teleport = (1.0 - damping) * inv_n;

    // global id -> (partition index, local owned index).
    let mut owner = vec![(usize::MAX, usize::MAX); n_global];
    for (pi, p) in parts.iter().enumerate() {
        for (li, &g) in p.owned_global_ids().iter().enumerate() {
            owner[g as usize] = (pi, li);
        }
    }

    // Per-partition local rank vectors (owned block then ghost block), init 1/n.
    let mut ranks: Vec<Vec<f32>> = parts
        .iter()
        .map(|p| vec![inv_n; p.local.n_nodes as usize])
        .collect();

    for _ in 0..iters {
        // Phase 1: each partition computes next owned ranks from current ranks
        // (owned + ghost), gathering over its local CSR.
        let mut next_owned: Vec<Vec<f32>> = Vec::with_capacity(parts.len());
        for (pi, p) in parts.iter().enumerate() {
            let r = &ranks[pi];
            let mut no = vec![0.0f32; p.n_owned as usize];
            for (v, slot) in no.iter_mut().enumerate() {
                let mut acc = 0.0f32;
                for e in p.local.offsets[v] as usize..p.local.offsets[v + 1] as usize {
                    let u_local = p.local.neighbors[e] as usize;
                    let u_global = p.global_ids[u_local] as usize;
                    acc += r[u_local] * inv_deg_global[u_global];
                }
                *slot = teleport + damping * acc;
            }
            next_owned.push(no);
        }
        // Commit owned ranks.
        for (pi, p) in parts.iter().enumerate() {
            ranks[pi][..p.n_owned as usize].copy_from_slice(&next_owned[pi]);
        }
        // Phase 2 (barrier): refresh each ghost from its owner's new owned rank.
        for pi in 0..parts.len() {
            let p = &parts[pi];
            for gi in p.n_owned as usize..p.local.n_nodes as usize {
                let g = p.global_ids[gi] as usize;
                let (opi, oli) = owner[g];
                let val = ranks[opi][oli];
                ranks[pi][gi] = val;
            }
        }
    }

    // Gather owned ranks into the global vector.
    let mut out = vec![0.0f32; n_global];
    for (pi, p) in parts.iter().enumerate() {
        for (li, &g) in p.owned_global_ids().iter().enumerate() {
            out[g as usize] = ranks[pi][li];
        }
    }
    out
}

/// GPU-per-partition distributed PageRank: identical decomposition + halo
/// exchange to [`distributed_pagerank`], but each partition's per-superstep
/// gather runs as a weighted [`gpu_spmv`] on the wgpu device — the unifying-
/// kernel view of the scale-out (distributed PageRank = partitioned GPU SpMV +
/// halo exchange). In-process all partitions share one device; the true
/// multi-GPU form gives each worker its own device + routes the boundary halo
/// over `TonicHaloTransport`.
///
/// The per-partition matrix `A_p` has `weights[e] = inv_deg_global[neighbor]`,
/// so `y = A_p · ranks` is exactly the PageRank pull-gather; we then apply
/// `teleport + damping·y` on the owned rows. (For brevity this rebuilds the SpMV
/// buffers each superstep; a production worker keeps `A_p` resident and only
/// re-uploads the rank vector.) Requires a GPU; returns `Err` without one.
pub fn distributed_pagerank_gpu(
    ctx: &EngineCtx,
    graph: &CsrGraph,
    n_partitions: u32,
    damping: f32,
    iters: u32,
) -> Result<Vec<f32>, String> {
    if ctx.gpu.is_none() {
        return Err("distributed_pagerank_gpu requires a wgpu device".to_string());
    }
    let n_global = graph.n_nodes as usize;
    if n_global == 0 {
        return Ok(Vec::new());
    }

    let parts = partition_csr(graph, None, n_partitions);

    let inv_deg_global: Vec<f32> = (0..n_global)
        .map(|v| {
            let d = graph.offsets[v + 1] - graph.offsets[v];
            if d == 0 {
                0.0
            } else {
                1.0 / d as f32
            }
        })
        .collect();

    let inv_n = 1.0 / n_global as f32;
    let teleport = (1.0 - damping) * inv_n;

    let mut owner = vec![(usize::MAX, usize::MAX); n_global];
    for (pi, p) in parts.iter().enumerate() {
        for (li, &g) in p.owned_global_ids().iter().enumerate() {
            owner[g as usize] = (pi, li);
        }
    }

    // Per-partition weighted matrix A_p (weights = neighbor's global inv-degree).
    // Structure is fixed across supersteps; only the rank vector `x` changes.
    let mats: Vec<WeightedCsr> = parts
        .iter()
        .map(|p| {
            let weights: Vec<f32> = p
                .local
                .neighbors
                .iter()
                .map(|&u_local| inv_deg_global[p.global_ids[u_local as usize] as usize])
                .collect();
            WeightedCsr {
                n_nodes: p.local.n_nodes,
                offsets: p.local.offsets.clone(),
                neighbors: p.local.neighbors.clone(),
                weights,
            }
        })
        .collect();

    let mut ranks: Vec<Vec<f32>> = parts
        .iter()
        .map(|p| vec![inv_n; p.local.n_nodes as usize])
        .collect();

    for _ in 0..iters {
        // Phase 1: each partition gathers via GPU SpMV, then applies teleport.
        let mut next_owned: Vec<Vec<f32>> = Vec::with_capacity(parts.len());
        for (pi, p) in parts.iter().enumerate() {
            let y = gpu_spmv(ctx, &mats[pi], &ranks[pi])?;
            let no: Vec<f32> = y[..p.n_owned as usize]
                .iter()
                .map(|&acc| teleport + damping * acc)
                .collect();
            next_owned.push(no);
        }
        for (pi, p) in parts.iter().enumerate() {
            ranks[pi][..p.n_owned as usize].copy_from_slice(&next_owned[pi]);
        }
        // Phase 2 (barrier): refresh ghosts from owners' new owned ranks.
        for pi in 0..parts.len() {
            let p = &parts[pi];
            for gi in p.n_owned as usize..p.local.n_nodes as usize {
                let g = p.global_ids[gi] as usize;
                let (opi, oli) = owner[g];
                let val = ranks[opi][oli];
                ranks[pi][gi] = val;
            }
        }
    }

    let mut out = vec![0.0f32; n_global];
    for (pi, p) in parts.iter().enumerate() {
        for (li, &g) in p.owned_global_ids().iter().enumerate() {
            out[g as usize] = ranks[pi][li];
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::cpu_pagerank;

    fn ring(n: u32) -> CsrGraph {
        let mut offsets = Vec::with_capacity((n + 1) as usize);
        let mut neighbors = Vec::new();
        for i in 0..n {
            offsets.push(neighbors.len() as u32);
            neighbors.push((i + n - 1) % n);
            neighbors.push((i + 1) % n);
        }
        offsets.push(neighbors.len() as u32);
        CsrGraph {
            n_nodes: n,
            offsets,
            neighbors,
        }
    }

    /// Deterministic connected, no-dangling graph: a ring + pseudo-random chords.
    fn chorded_ring(n: u32) -> CsrGraph {
        use std::collections::BTreeSet;
        let mut edges: BTreeSet<(u32, u32)> = BTreeSet::new();
        for i in 0..n {
            let a = i;
            let b = (i + 1) % n;
            edges.insert((a.min(b), a.max(b)));
            let c = a.wrapping_mul(31).wrapping_add(7) % n;
            if a != c {
                edges.insert((a.min(c), a.max(c)));
            }
        }
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n as usize];
        for (u, v) in edges {
            adj[u as usize].push(v);
            adj[v as usize].push(u);
        }
        let mut offsets = vec![0u32];
        let mut neighbors = Vec::new();
        for a in &adj {
            neighbors.extend_from_slice(a);
            offsets.push(neighbors.len() as u32);
        }
        CsrGraph {
            n_nodes: n,
            offsets,
            neighbors,
        }
    }

    /// The whole point: distributed (P partitions) == single-process, for any P.
    #[test]
    fn distributed_matches_single_process() {
        for g in [ring(60), chorded_ring(120)] {
            let single = cpu_pagerank(&g, 0.85, 80);
            for p in [1u32, 2, 3, 5, 8] {
                let dist = distributed_pagerank(&g, p, 0.85, 80);
                assert_eq!(dist.len(), single.len());
                let max_dev = dist
                    .iter()
                    .zip(single.iter())
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0f32, f32::max);
                assert!(
                    max_dev < 1e-4,
                    "n={} p={p}: distributed vs single max dev {max_dev}",
                    g.n_nodes
                );
                // Mass conserved across partitions.
                let mass: f64 = dist.iter().map(|&r| r as f64).sum();
                assert!((mass - 1.0).abs() < 1e-3, "p={p}: mass {mass}");
            }
        }
    }

    #[test]
    fn single_partition_is_plain_pagerank() {
        let g = ring(40);
        let dist = distributed_pagerank(&g, 1, 0.85, 50);
        let single = cpu_pagerank(&g, 0.85, 50);
        for (a, b) in dist.iter().zip(single.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}
