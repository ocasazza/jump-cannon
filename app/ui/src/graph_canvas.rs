//! Graph view: data bootstrap + the wgpu canvas component.
//!
//! Second-generation renderer for the Dioxus shell — the Canvas2D
//! placeholder is gone. The actual pixels come from `crate::render`
//! (the wgpu pipeline port from `crates/graph-renderer`): same WGSL
//! shaders, same 6DoF camera, same in-process GPU force layout. This
//! module owns (a) the one-shot fetch that turns graph-api responses
//! into the renderer's buffer seed, and (b) the `<canvas>` element with
//! its interaction handlers (drag-rotate / wheel-zoom / click-pick).

use std::collections::HashMap;

use dioxus::events::{MouseEvent, WheelEvent};
use dioxus::html::geometry::WheelDelta;
use dioxus::prelude::*;

use crate::api;
use crate::render;

/// Everything the app needs, fetched once from graph-api.
#[derive(Clone, PartialEq)]
pub struct GraphData {
    /// Server topology identity. `None` is reserved for browser-owned graphs
    /// which graph-api and graph-compute cannot safely address.
    pub graph_revision: Option<u64>,
    pub n_nodes: u32,
    pub n_edges: u32,
    pub num_communities: u32,
    pub num_wcc: u32,
    /// Node ids, same order as the renderer's buffers.
    pub ids: Vec<String>,
    pub id_to_idx: HashMap<String, u32>,
    /// Seed buffers for `render::mount_canvas` (positions / edges /
    /// colors / sizes in renderer wire format).
    pub scene: render::Scene,
}

/// Fetch the full graph bundle and derive the GPU buffer seed. Any piece
/// failing fails the load (the caller retries — the server may still be
/// indexing the vault).
///
/// Mirrors the egui app's bootstrap (`app.rs::spawn_fetch_task` +
/// `try_promote_bootstrap_to_gpu`):
///   - when the importer authored positions (`Init.positions_authored`,
///     e.g. an SDF 2D depiction), the sim seeds from them — recentered and
///     rescaled so the mean edge length matches the spring length — and the
///     structure survives to first paint;
///   - otherwise nodes seed on a hollow sphere shell (radius 800 wu), then
///     the multilevel coarsening warm-up (`graph_layouts::warmup_positions`)
///     replaces that with a coarsened-cascade seed so the GPU sim converges
///     in a handful of frames instead of hundreds;
///   - colors come from the community metric through the Tableau20
///     palette (egui default `ColorBy::Community`);
///   - sizes come from pagerank with the default 0.5 multiplier
///     (egui default `SizeBy::PageRank`, `size_mul = 0.5`).
pub async fn load() -> Result<GraphData, String> {
    let init = api::init().await?;
    let ids_response = api::revisioned_ids().await?;
    let edges_response = api::revisioned_edges().await?;
    let ids = ids_response.value;
    let edges = edges_response.value;
    let revision = init.graph_revision;

    // Count checks cannot detect a same-cardinality graph swap. When the
    // server supports revisions, require every independently fetched buffer
    // to belong to the same topology before mounting it.
    for (name, got) in [
        ("ids", ids_response.revision),
        ("edges", edges_response.revision),
    ] {
        if revision != 0 && got != 0 && got != revision {
            return Err(format!(
                "inconsistent snapshot ({name} revision {got}, init revision {revision}) — \
                 server graph changed mid-load"
            ));
        }
    }

    let n = init.n_nodes as usize;
    // The three fetches above aren't atomic: a server-side graph swap
    // (vault reload, /generate) between them hands us an edge list that
    // doesn't match `n`. Fail the attempt — the caller's retry loop picks
    // up the new snapshot consistently a moment later.
    if ids.len() != n || edges.iter().any(|&e| e as usize >= n) {
        return Err(format!(
            "inconsistent snapshot (n={n}, ids={}, max edge idx={:?}) — server graph changed mid-load",
            ids.len(),
            edges.iter().max()
        ));
    }
    let mut metrics: HashMap<String, Vec<f32>> = HashMap::new();
    for name in ["community", "pagerank"] {
        match api::revisioned_metric(name).await {
            Ok(r) if revision == 0 || r.revision == 0 || r.revision == revision => {
                metrics.insert(name.to_string(), r.value);
            }
            Ok(r) => {
                return Err(format!(
                    "inconsistent snapshot (metric {name} revision {}, init revision {revision}) — \
                     server graph changed mid-load",
                    r.revision
                ));
            }
            Err(e) => tracing::warn!("[graph] metric {name}: {e}"),
        }
    }

    // Typed-attribute contract (molecular graphs): UFF per-edge rest
    // lengths and per-node repulsion weights. Any failure or inconsistency
    // disables the feature — the sim then behaves exactly as before on
    // the global spring length / Coulomb strength.
    let typed = match (
        api::revisioned_node_types().await,
        api::revisioned_edge_kinds().await,
    ) {
        (Ok(types), Ok(kinds))
            if (revision == 0 || types.revision == 0 || types.revision == revision)
                && (revision == 0 || kinds.revision == 0 || kinds.revision == revision) =>
        {
            typed_force_params(&types, &kinds, &edges, n)
        }
        (Ok(types), Ok(kinds)) => {
            return Err(format!(
                "inconsistent snapshot (types revision {}, kinds revision {}, init revision {revision}) — \
                 server graph changed mid-load",
                types.revision, kinds.revision,
            ));
        }
        _ => {
            *TYPED_FORCE_SUMMARY.write() = None;
            TypedForceParams {
                edge_rest: None,
                node_repulsion: None,
            }
        }
    };

    // Data-owned anchor (spec E1): when the graph carries UFF rests, the
    // authored-coordinate rescale anchors at their mean — the same value
    // the regime resolver fills a data-owned `spring_len` with — instead
    // of a vault-tuned global slider value.
    let spring_len = typed
        .edge_rest
        .as_deref()
        .and_then(crate::panels::regimes::mean_typed_rest)
        .unwrap_or_else(|| crate::panels::layout::active_spring_len(n));
    let positions = if init.positions_authored {
        // Importer-authored coordinates (e.g. an SDF 2D depiction): the
        // structure is the layout. Seed from them — centered and rescaled so
        // the mean bond length matches the sim's spring length — and skip
        // the sphere/warm-up, which would scramble the authored structure.
        let r = api::revisioned_positions().await?;
        if revision != 0 && r.revision != 0 && r.revision != revision {
            return Err(format!(
                "inconsistent snapshot (positions revision {}, init revision {revision}) — \
                 server graph changed mid-load",
                r.revision
            ));
        }
        authored_positions(&r.value, &edges, n, spring_len)
            .ok_or_else(|| "positions buffer does not match node count".to_string())?
    } else {
        // Sphere shell seed, then the coarsening warm-up (which always
        // returns a full position set, so it effectively rules; the sphere
        // remains as the fallback should warmup ever come back short).
        //
        // Skip the warmup for large graphs (>10k nodes): the multilevel
        // coarsening + CPU FR cascade runs in WASM on the main thread and
        // blocks the UI for seconds at 100k scale. The sphere shell seed is
        // perfectly adequate when a GPU compute backend (graph-compute) is
        // handling layout — the GPU converges from any reasonable init.
        let mut positions = render::data::spawn_on_unit_sphere(n, 800.0);
        if n <= 10_000 {
            let warmed = graph_layouts::warmup_positions(n, &edges, spring_len, 0xC0A75E);
            if warmed.len() == positions.len() {
                positions = warmed;
            }
        }
        positions
    };

    let colors = render::data::colors_from_metric("community", &metrics, n);
    let sizes = render::data::sizes_from_metric("pagerank", &metrics, n, 0.5);

    let id_to_idx = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i as u32))
        .collect();

    let data = GraphData {
        graph_revision: (revision != 0).then_some(revision),
        n_nodes: init.n_nodes,
        n_edges: init.n_edges,
        num_communities: init.num_communities,
        num_wcc: init.num_wcc,
        ids,
        id_to_idx,
        scene: render::Scene {
            positions,
            edges,
            colors,
            sizes,
            edge_rest: typed.edge_rest,
            node_repulsion: typed.node_repulsion,
        },
    };
    // Resolve the layout regime (and auto-apply molecular options) before
    // the graph commits — the canvas mount then boots the host from the
    // regime's persisted settings (see render::mount_canvas).
    crate::panels::regimes::on_graph_loaded(&data);
    Ok(data)
}

/// Summary of the typed (molecular) force parameters the last graph load
/// resolved — `(typed nodes, typed edges)`. Feeds the layout regime
/// resolver's `typed_bond_coverage` predicate (panels/regimes.rs, spec M4:
/// coverage = typed_edges / edge_count) and the Layout panel's provenance
/// rows. `None` = feature inactive.
pub(crate) static TYPED_FORCE_SUMMARY: GlobalSignal<Option<(usize, usize)>> =
    Signal::global(|| None);

/// Typed force parameters from the typed-attribute wire: per-edge UFF
/// rest lengths plus per-node UFF repulsion weights. `None` unless at
/// least one item resolves — untyped graphs skip the feature entirely
/// and keep the global spring length / Coulomb strength. A `0.0` entry
/// means "no typed value for this item" (unknown kind, unknown element,
struct TypedForceParams {
    edge_rest: Option<Vec<f32>>,
    node_repulsion: Option<Vec<f32>>,
}

fn typed_force_params(
    types: &api::TypeTable,
    kinds: &api::TypeTable,
    edges: &[u32],
    n: usize,
) -> TypedForceParams {
    let n_edges = edges.len() / 2;
    if kinds.per_item.len() != n_edges || types.per_item.len() != n {
        // Mid-load swap the revision check did not catch (or an older
        // server): never mount a misaligned table.
        return TypedForceParams {
            edge_rest: None,
            node_repulsion: None,
        };
    }
    let element = |node: u32| -> Option<&str> {
        let idx = *types.per_item.get(node as usize)?;
        if idx == u32::MAX {
            return None;
        }
        types.table.get(idx as usize).map(String::as_str)
    };

    // Per-node repulsion weights (UFF well depth relative to carbon).
    let mut any_weight = false;
    let mut weights = vec![0.0f32; n];
    for (node, slot) in weights.iter_mut().enumerate() {
        if let Some(w) = element(node as u32).and_then(graph_layouts::uff::repulsion_weight) {
            *slot = w;
            any_weight = true;
        }
    }

    // Per-edge spring rest lengths (UFF bond geometry).
    let mut any_rest = false;
    let mut rests = vec![0.0f32; n_edges];
    if !kinds.table.is_empty() {
        for (e, slot) in rests.iter_mut().enumerate() {
            let kind_idx = kinds.per_item[e];
            if kind_idx == u32::MAX {
                continue;
            }
            let Some(order) = kinds
                .table
                .get(kind_idx as usize)
                .and_then(|kind| graph_layouts::uff::bond_order(kind))
            else {
                continue;
            };
            let (Some(a), Some(b)) = (element(edges[2 * e]), element(edges[2 * e + 1])) else {
                continue;
            };
            if let Some(rest) = graph_layouts::uff::bond_rest_length(a, b, order) {
                *slot = rest;
                any_rest = true;
            }
        }
    }

    *TYPED_FORCE_SUMMARY.write() = (any_weight || any_rest).then(|| {
        (
            weights.iter().filter(|w| **w > 0.0).count(),
            rests.iter().filter(|r| **r > 0.0).count(),
        )
    });
    TypedForceParams {
        edge_rest: any_rest.then_some(rests),
        node_repulsion: any_weight.then_some(weights),
    }
}

/// Importer-authored 2D coordinates → sim seed: z = 0, recentered on the
/// centroid, and uniformly rescaled so the mean EDGE length matches the
/// force sim's spring length. The scale step is what keeps an SDF depiction
/// (ångström units, ±5) and a vault circle (radius ~200) equally usable:
/// shape and bond-length ratios are authored data, absolute units are not.
/// `flat` is `[x0, y0, x1, y1, …]`; returns `None` when it doesn't hold `n`
/// points or every bond is degenerate.
fn authored_positions(flat: &[f32], edges: &[u32], n: usize, spring_len: f32) -> Option<Vec<f32>> {
    if flat.len() != n * 2 || n == 0 {
        return None;
    }
    let (mut cx, mut cy) = (0.0_f32, 0.0_f32);
    for i in 0..n {
        cx += flat[2 * i];
        cy += flat[2 * i + 1];
    }
    cx /= n as f32;
    cy /= n as f32;

    let mut mean_len = 0.0_f32;
    let mut counted = 0u32;
    for pair in edges.chunks_exact(2) {
        let (a, b) = (pair[0] as usize, pair[1] as usize);
        if a >= n || b >= n {
            continue;
        }
        let dx = flat[2 * a] - flat[2 * b];
        let dy = flat[2 * a + 1] - flat[2 * b + 1];
        let len = (dx * dx + dy * dy).sqrt();
        if len > 1e-6 {
            mean_len += len;
            counted += 1;
        }
    }
    let scale = if counted > 0 {
        spring_len / (mean_len / counted as f32)
    } else {
        1.0
    };

    let mut positions = Vec::with_capacity(n * 3);
    for i in 0..n {
        positions.push((flat[2 * i] - cx) * scale);
        positions.push((flat[2 * i + 1] - cy) * scale);
        positions.push(0.0);
    }
    Some(positions)
}

/// Convert an embedded world's materialized snapshot into `GraphData`,
/// mirroring the Generate panel's client-graph path (`graph_revision: None`,
/// default colors/sizes, union-find `num_wcc`, no Louvain). Node iteration
/// order is the snapshot's `BTreeMap` order, so the same snapshot always
/// mounts the same buffer layout. Positions come from the stored `x`/`y`
/// when any node carries them; otherwise the same deterministic sphere +
/// coarsening warm-up as `load()` seeds the sim.
pub(crate) fn graph_data_from_snapshot(snapshot: &graph_vcs::Snapshot) -> GraphData {
    // World snapshots carry no typed attributes — the molecular force
    // parameters (and their Layout-tab summary) are inactive here.
    *TYPED_FORCE_SUMMARY.write() = None;
    let mut id_to_idx: HashMap<String, u32> = HashMap::with_capacity(snapshot.nodes.len());
    let mut ids: Vec<String> = Vec::with_capacity(snapshot.nodes.len());
    for id in snapshot.nodes.keys() {
        id_to_idx.insert(id.0.clone(), ids.len() as u32);
        ids.push(id.0.clone());
    }
    let n = ids.len();

    let mut edges: Vec<u32> = Vec::with_capacity(snapshot.edges.len() * 2);
    for edge in &snapshot.edges {
        // Mirror the Generate panel: edges whose endpoints are gone are
        // silently dropped (`DeleteNode` does not cascade).
        let (Some(&s), Some(&t)) = (
            id_to_idx.get(&edge.source),
            id_to_idx.get(&edge.target),
        ) else {
            continue;
        };
        edges.push(s);
        edges.push(t);
    }
    let n_edges = (edges.len() / 2) as u32;

    let has_stored_positions = snapshot
        .nodes
        .values()
        .any(|node| node.x != 0.0 || node.y != 0.0);
    let positions = if has_stored_positions {
        let mut flat: Vec<f32> = Vec::with_capacity(n * 2);
        for node in snapshot.nodes.values() {
            flat.push(node.x);
            flat.push(node.y);
        }
        let spring_len = crate::panels::layout::active_spring_len(n);
        authored_positions(&flat, &edges, n, spring_len)
            .unwrap_or_else(|| render::data::spawn_on_unit_sphere(n, 800.0))
    } else {
        let mut positions = render::data::spawn_on_unit_sphere(n, 800.0);
        if n <= 10_000 {
            let spring_len = crate::panels::layout::active_spring_len(n);
            let warmed = graph_layouts::warmup_positions(n, &edges, spring_len, 0xC0A75E);
            if warmed.len() == positions.len() {
                positions = warmed;
            }
        }
        positions
    };

    let metrics: HashMap<String, Vec<f32>> = HashMap::new();
    let colors = render::data::colors_from_metric("community", &metrics, n);
    let sizes = render::data::sizes_from_metric("pagerank", &metrics, n, 0.5);
    let num_wcc = crate::panels::generate::wcc_count(n, &edges);

    let data = GraphData {
        graph_revision: None,
        n_nodes: n as u32,
        n_edges,
        num_communities: 0,
        num_wcc,
        ids,
        id_to_idx,
        scene: render::Scene {
            positions,
            edges,
            colors,
            sizes,
            edge_rest: None,
            node_repulsion: None,
        },
    };
    // Untyped graph → catch-all regime (restores any displaced settings).
    crate::panels::regimes::on_graph_loaded(&data);
    data
}
/// Convert a [`vault_data::VaultGraph`] into [`GraphData`], mirroring
/// [`graph_data_from_snapshot`] for the github-import panel's browser-only
/// path. Node iteration order follows the `IndexMap`'s insertion order for
/// determinism; nodes carry no stored positions (x/y are 0.0), so we always
/// seed from the sphere + coarsening warm-up.
pub(crate) fn graph_data_from_vault(graph: &vault_data::VaultGraph) -> GraphData {
    let mut id_to_idx: HashMap<String, u32> = HashMap::with_capacity(graph.nodes.len());
    let mut ids: Vec<String> = Vec::with_capacity(graph.nodes.len());
    for id in graph.nodes.keys() {
        id_to_idx.insert(id.clone(), ids.len() as u32);
        ids.push(id.clone());
    }
    let n = ids.len();

    // Per-edge UFF rest lengths ride alongside the index buffer so both
    // apply the same dropped-endpoint filter. A 0.0 entry means "untyped
    // or unknown geometry" — the sim falls back to the global spring
    // length for that edge.
    let mut edge_rest: Vec<f32> = Vec::with_capacity(graph.edges.len());
    let mut any_rest = false;
    let mut edges: Vec<u32> = Vec::with_capacity(graph.edges.len() * 2);
    for edge in &graph.edges {
        // Drop edges whose endpoints are gone (shouldn't happen for a
        // freshly-extracted graph, but mirrors the snapshot path).
        let (Some(&s), Some(&t)) = (id_to_idx.get(&edge.source), id_to_idx.get(&edge.target))
        else {
            continue;
        };
        edges.push(s);
        edges.push(t);
        let rest = edge
            .kind
            .as_deref()
            .and_then(graph_layouts::uff::bond_order)
            .and_then(|order| {
                let a = graph.nodes.get(&edge.source)?.meta.doctype.as_deref()?;
                let b = graph.nodes.get(&edge.target)?.meta.doctype.as_deref()?;
                graph_layouts::uff::bond_rest_length(a, b, order)
            })
            .unwrap_or(0.0);
        if rest > 0.0 {
            any_rest = true;
        }
        edge_rest.push(rest);
    }
    // Per-node UFF repulsion weights, aligned with `ids` (the IndexMap's
    // insertion order — same iteration as `id_to_idx` above). A 0.0
    // entry means "untyped element" — weight-1.0 fallback in the sim.
    let mut any_weight = false;
    let mut node_repulsion: Vec<f32> = Vec::with_capacity(n);
    for node in graph.nodes.values() {
        let w = node
            .meta
            .doctype
            .as_deref()
            .and_then(graph_layouts::uff::repulsion_weight)
            .unwrap_or(0.0);
        if w > 0.0 {
            any_weight = true;
        }
        node_repulsion.push(w);
    }
    let typed_nodes = node_repulsion.iter().filter(|w| **w > 0.0).count();
    let typed_edges = edge_rest.iter().filter(|r| **r > 0.0).count();
    *TYPED_FORCE_SUMMARY.write() = (any_weight || any_rest).then_some((typed_nodes, typed_edges));
    let node_repulsion = any_weight.then_some(node_repulsion);
    let edge_rest = any_rest.then_some(edge_rest);
    let n_edges = (edges.len() / 2) as u32;
    // No stored positions — seed from sphere + warmup, same as the snapshot
    // path. Typed graphs anchor the warmup at the mean UFF rest (spec E1).
    let mut positions = render::data::spawn_on_unit_sphere(n, 800.0);
    if n <= 10_000 {
        let spring_len = edge_rest
            .as_deref()
            .and_then(crate::panels::regimes::mean_typed_rest)
            .unwrap_or_else(|| crate::panels::layout::active_spring_len(n));
        let warmed = graph_layouts::warmup_positions(n, &edges, spring_len, 0xC0A75E);
        if warmed.len() == positions.len() {
            positions = warmed;
        }
    }

    let metrics: HashMap<String, Vec<f32>> = HashMap::new();
    let colors = render::data::colors_from_metric("community", &metrics, n);
    let sizes = render::data::sizes_from_metric("pagerank", &metrics, n, 0.5);
    let num_wcc = crate::panels::generate::wcc_count(n, &edges);

    let data = GraphData {
        graph_revision: None,
        n_nodes: n as u32,
        n_edges,
        num_communities: 0,
        num_wcc,
        ids,
        id_to_idx,
        scene: render::Scene {
            positions,
            edges,
            colors,
            sizes,
            edge_rest,
            node_repulsion,
        },
    };
    crate::panels::regimes::on_graph_loaded(&data);
    data
}

/// In-flight pointer drag (camera rotate). A press that never travels
/// more than the slop is a click — and clicks pick nodes.
#[derive(Clone, Copy, PartialEq)]
struct Drag {
    last_mx: f64,
    last_my: f64,
    moved: bool,
}

/// The canvas element + interaction handlers. Pixels come from the rAF
/// loop in `crate::render`; handlers only steer the camera and selection.
///
/// Input map (matching the egui app's semantics):
///   - mouse-drag rotates pitch + yaw (any button; sensitivity + curve
///     from workspace.rs)
///   - wheel zooms along the camera forward axis (distance-aware)
///   - click (no travel) picks the nearest node within tolerance and sets
///     the app `selected` signal
///   - plain mousemove updates hover (white rim / edge brighten)
///   - WASDQE pan is handled at the workspace root (see main.rs) and
///     gated on the pointer being over this canvas
#[component]
pub fn GraphCanvas(graph: Signal<Option<GraphData>>, selected: Signal<Option<String>>) -> Element {
    let mut drag = use_signal(|| Option::<Drag>::None);
    let render_status = render::RENDER_STATUS.read().clone();
    let render_state = render_status.as_attr();
    let node_count = graph.read().as_ref().map(|g| g.n_nodes).unwrap_or(0);

    // One owner for renderer initialization. This effect runs after the DOM
    // commit, reacts when the graph scene changes, and runs again when a
    // minimized Graph panel is restored. Keeping it here avoids racing a
    // graph-commit mount against the canvas onmounted hook.
    use_effect(move || {
        if let Some(g) = graph.read().as_ref() {
            render::mount_canvas(g.scene.clone());
        }
    });

    rsx! {
        div {
            class: "graph-wrap",
            "data-render-state": "{render_state}",
            canvas {
                id: render::CANVAS_ID,
                class: "graph-canvas",
                "data-node-count": "{node_count}",
                onmousedown: move |e: MouseEvent| {
                    let c = e.element_coordinates();
                    drag.set(Some(Drag { last_mx: c.x, last_my: c.y, moved: false }));
                },
                onmousemove: move |e: MouseEvent| {
                    render::set_pointer_over(true);
                    let c = e.element_coordinates();
                    // A drag is only live while a button is held — without
                    // this check a press whose release happened off-canvas
                    // leaves a stale drag that spins the camera on re-entry.
                    if drag.read().is_some() && e.held_buttons().is_empty() {
                        drag.set(None);
                    }
                    let cur = *drag.read();
                    if let Some(mut d) = cur {
                        let (dx, dy) = (c.x - d.last_mx, c.y - d.last_my);
                        if d.moved || dx.abs() + dy.abs() > 3.0 {
                            d.moved = true;
                            render::pointer_rotate(dx as f32, dy as f32);
                        }
                        d.last_mx = c.x;
                        d.last_my = c.y;
                        drag.set(Some(d));
                    } else {
                        // Hover pipeline (header hint + shader rim + focus
                        // dim) — throttle/hold policy lives there.
                        crate::anchored::hover_at(c.x as f32, c.y as f32);
                    }
                },
                onmouseup: move |e: MouseEvent| {
                    let was = *drag.read();
                    drag.set(None);
                    // A press that never travelled is a click. The anchored
                    // module owns the egui click semantics: node hit →
                    // sticky focus (and we mirror the hit into `selected`
                    // for the Inspector/Document panels, like the egui
                    // `selected_node_idx`); empty canvas → clear sticky
                    // focus, `selected` untouched.
                    if let Some(d) = was {
                        if !d.moved {
                            let c = e.element_coordinates();
                            let hit_id = crate::anchored::canvas_click(c.x as f32, c.y as f32)
                                .and_then(|i| {
                                    graph.read().as_ref().and_then(|g| g.ids.get(i as usize).cloned())
                                });
                            if hit_id.is_some() {
                                selected.set(hit_id);
                            }
                        }
                    }
                },
                onmouseenter: move |_| render::set_pointer_over(true),
                onmouseleave: move |_| {
                    drag.set(None);
                    render::set_pointer_over(false);
                    // Edge hover clears immediately; node hover holds for
                    // 250 ms (egui update_hover_focus's pointer-None arm).
                    crate::anchored::canvas_leave();
                },
                // RMB must stay available as a rotate button (egui app
                // rotates on RMB/MMB drag) — suppress the context menu.
                oncontextmenu: move |e| e.prevent_default(),
                onwheel: move |e: WheelEvent| {
                    e.prevent_default();
                    let dy = match e.delta() {
                        WheelDelta::Pixels(p) => p.y,
                        WheelDelta::Lines(l) => l.y * 40.0,
                        WheelDelta::Pages(p) => p.y * 400.0,
                    };
                    // Browser wheel-down is +y; egui zoom-in is positive.
                    render::wheel_zoom(-dy as f32);
                },
            }
            if render_status == render::RenderStatus::Initializing {
                div {
                    class: "graph-render-status initializing",
                    "data-testid": "graph-render-status",
                    "Preparing WebGPU renderer…"
                }
            }
            if let render::RenderStatus::Unavailable { title, detail } = render_status {
                div {
                    class: "graph-render-status unavailable",
                    role: "alert",
                    "data-testid": "graph-render-status",
                    h2 { "{title}" }
                    p { "{detail}" }
                }
            }
        }
    }
}
