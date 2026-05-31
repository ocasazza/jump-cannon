//! Spectral (Fiedler) layout — a one-shot CPU **seed** that places clusters
//! cleanly separated, ideal as the initial positions for a force/geometric
//! refinement of a small-world graph.
//!
//! Coordinates come from the eigenvectors of the smallest non-zero eigenvalues
//! of the graph Laplacian `L = D − A` (Koren, *Drawing Graphs by Eigenvectors*,
//! 2005): the 2nd-smallest eigenvector ("Fiedler vector") is the best single
//! drawing axis, the 3rd-smallest the next, and so on. The all-ones vector at
//! `λ = 0` is the degenerate solution and is projected out.
//!
//! We avoid pulling in an eigensolver dependency by using **deflated power
//! iteration** on `B = c·I − L` (so `B`'s *largest* eigenvectors are `L`'s
//! *smallest*), Gram–Schmidt-orthogonalising each iterate against the constant
//! vector and the axes already found. Power iteration converges geometrically
//! with the spectral gap — which is exactly *large* for graphs that have
//! well-separated clusters, so this is fast on the inputs that need it most.
//!
//! ⚠️ This is a *seed*, not a force/stress energy. The Laplacian quadratic form
//! `xᵀLx` is **not** the force-layout edge-length objective — see
//! `docs/small-world-layout-research.md`. Run a force/geometric layout afterward
//! to refine.

use serde::{Deserialize, Serialize};

use crate::layout::layout_trait::{LayoutDescriptor, LayoutKind, LayoutRequirements, StaticLayout};
use crate::types::Graph;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpectralSettings {
    /// Half-extent each axis is scaled to (max |coord| ≈ `radius`).
    pub radius: f32,
    /// Power-iteration steps per axis. More = tighter convergence; clustered
    /// graphs (large spectral gap) need few. Default is generous.
    pub iterations: u32,
    /// Emit a third (z) Fiedler axis for a 3D seed. When false, z = 0.
    pub three_d: bool,
}

impl Default for SpectralSettings {
    fn default() -> Self {
        Self {
            radius: 300.0,
            iterations: 200,
            three_d: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SpectralLayout;

impl StaticLayout for SpectralLayout {
    type Settings = SpectralSettings;

    fn descriptor() -> LayoutDescriptor {
        LayoutDescriptor {
            id: "spectral",
            kind: LayoutKind::Static,
            display_name: "Spectral (Fiedler)",
            description: "Eigenvector seed that separates clusters; use as the \
                          starting positions for a force/geometric refinement.",
            requirements: LayoutRequirements {
                needs_edges: true,
                needs_cpu_positions: false,
                needs_gpu_positions_buffer: true,
            },
        }
    }

    fn solve(settings: &Self::Settings, graph: &Graph) -> Result<Vec<f32>, String> {
        // Stable node order (matches the other static layouts' sorted-key order).
        let mut node_order: Vec<&String> = graph.nodes.keys().collect();
        node_order.sort();
        let n = node_order.len();
        if n == 0 {
            return Ok(Vec::new());
        }

        let index: std::collections::HashMap<&String, usize> =
            node_order.iter().enumerate().map(|(i, &id)| (id, i)).collect();

        // Undirected adjacency (dedup parallel edges + self-loops) as CSR-ish
        // neighbour lists; degrees for the Laplacian diagonal.
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut seen = std::collections::HashSet::new();
        for edge in graph.edges.values() {
            let (Some(&s), Some(&t)) = (index.get(&edge.source), index.get(&edge.target)) else {
                continue;
            };
            if s == t {
                continue;
            }
            let key = if s < t { (s, t) } else { (t, s) };
            if seen.insert(key) {
                adj[s].push(t);
                adj[t].push(s);
            }
        }
        let degree: Vec<f32> = adj.iter().map(|a| a.len() as f32).collect();

        // Shift so power iteration on B = c·I − L finds L's SMALLEST eigenvectors.
        // λ_max(L) ≤ 2·d_max, so c = 2·d_max + 1 keeps every B-eigenvalue > 0 and
        // preserves the L-ordering under the c − λ map.
        let d_max = degree.iter().cloned().fold(0.0_f32, f32::max);
        let c = 2.0 * d_max + 1.0;

        let want = if settings.three_d { 3 } else { 2 };
        let iters = settings.iterations.max(1);

        // Basis to deflate against: u0 = normalised constant vector (the λ=0
        // eigenvector for a connected graph), then each axis we extract.
        let mut basis: Vec<Vec<f32>> = Vec::with_capacity(want + 1);
        basis.push(normalized(vec![1.0; n]));

        let mut axes: Vec<Vec<f32>> = Vec::with_capacity(want);
        for k in 0..want {
            // Deterministic, axis-distinct start (no rand dependency).
            let mut x = seeded_vector(n, 0x51E5_u64.wrapping_add(k as u64));
            for _ in 0..iters {
                orthogonalize(&mut x, &basis);
                // y = B·x = c·x − L·x, where (L·x)_i = deg_i·x_i − Σ_{j∈N(i)} x_j.
                let mut y = vec![0.0f32; n];
                for i in 0..n {
                    let mut neighbour_sum = 0.0f32;
                    for &j in &adj[i] {
                        neighbour_sum += x[j];
                    }
                    let lx = degree[i] * x[i] - neighbour_sum;
                    y[i] = c * x[i] - lx;
                }
                x = normalized(y);
            }
            orthogonalize(&mut x, &basis);
            x = normalized(x);
            basis.push(x.clone());
            axes.push(x);
        }

        // Scale each axis to ±radius by its peak magnitude, then pack [x,y,z].
        let scaled: Vec<Vec<f32>> = axes
            .iter()
            .map(|axis| {
                let peak = axis.iter().fold(0.0f32, |m, &v| m.max(v.abs())).max(1e-6);
                let s = settings.radius / peak;
                axis.iter().map(|&v| v * s).collect()
            })
            .collect();

        let mut out = Vec::with_capacity(n * 3);
        for i in 0..n {
            out.push(scaled.first().map_or(0.0, |a| a[i]));
            out.push(scaled.get(1).map_or(0.0, |a| a[i]));
            out.push(scaled.get(2).map_or(0.0, |a| a[i]));
        }
        Ok(out)
    }
}

/// L2-normalise in place-by-value; a zero vector is returned unchanged.
fn normalized(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Gram–Schmidt: subtract the projection of `x` onto every (assumed
/// unit-norm) basis vector, so `x ⟂ span(basis)`.
fn orthogonalize(x: &mut [f32], basis: &[Vec<f32>]) {
    for b in basis {
        let dot: f32 = x.iter().zip(b).map(|(xi, bi)| xi * bi).sum();
        for (xi, bi) in x.iter_mut().zip(b) {
            *xi -= dot * bi;
        }
    }
}

/// Deterministic start vector (xorshift, same generator the coarsen module
/// uses) so layouts are reproducible without a `rand` dependency.
fn seeded_vector(n: usize, seed: u64) -> Vec<f32> {
    let mut s = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut next = move || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let v = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    };
    (0..n).map(|_| next()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Edge, Node};

    /// Two K4 cliques joined by a single bridge edge (a "barbell"). The Fiedler
    /// vector must split them: every node of one clique on one side of 0 on the
    /// primary axis, every node of the other clique on the opposite side.
    fn barbell() -> Graph {
        let mut g = Graph::new();
        for id in ["a0", "a1", "a2", "a3", "b0", "b1", "b2", "b3"] {
            g.add_node(Node::new(id));
        }
        let mut e = 0;
        let mut add = |g: &mut Graph, s: &str, t: &str, e: &mut i32| {
            g.add_edge(Edge::new(format!("e{e}"), s, t));
            *e += 1;
        };
        for clique in [["a0", "a1", "a2", "a3"], ["b0", "b1", "b2", "b3"]] {
            for i in 0..clique.len() {
                for j in (i + 1)..clique.len() {
                    add(&mut g, clique[i], clique[j], &mut e);
                }
            }
        }
        add(&mut g, "a0", "b0", &mut e); // bridge
        g
    }

    #[test]
    fn fiedler_axis_separates_the_two_cliques() {
        let g = barbell();
        let out = SpectralLayout::solve(&SpectralSettings::default(), &g).unwrap();
        assert_eq!(out.len(), 8 * 3);

        // Sorted node order is a0..a3, b0..b3 → indices 0..4 vs 4..8. Read the
        // primary (x) axis and check the two cliques sit on opposite signs.
        let x = |i: usize| out[i * 3];
        let a_mean = (0..4).map(x).sum::<f32>() / 4.0;
        let b_mean = (4..8).map(x).sum::<f32>() / 4.0;
        assert!(
            a_mean * b_mean < 0.0,
            "cliques should land on opposite sides of the Fiedler axis: a={a_mean}, b={b_mean}"
        );
        // And every node of a clique on its own side (clean separation).
        assert!((0..4).all(|i| x(i).signum() == a_mean.signum()));
        assert!((4..8).all(|i| x(i).signum() == b_mean.signum()));
    }

    #[test]
    fn empty_and_singleton_do_not_panic() {
        assert!(SpectralLayout::solve(&SpectralSettings::default(), &Graph::new())
            .unwrap()
            .is_empty());
        let mut g = Graph::new();
        g.add_node(Node::new("solo"));
        let out = SpectralLayout::solve(&SpectralSettings::default(), &g).unwrap();
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn output_respects_radius_bound() {
        let g = barbell();
        let mut s = SpectralSettings::default();
        s.radius = 50.0;
        let out = SpectralLayout::solve(&s, &g).unwrap();
        // Peak magnitude on each populated axis ≈ radius; never exceeds it.
        for coord in &out {
            assert!(coord.abs() <= 50.0 + 1e-3, "coord {coord} exceeds radius");
        }
    }
}
