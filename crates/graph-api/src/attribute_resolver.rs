use crate::state::GraphSnapshot;
use graph_compute::engines::geometric::{
    ClassSource, CoordinationSource, EdgeLengthSource, GeometricSettings, MassSource,
};
use graph_compute::engines::GraphAttributes;
use graph_layouts::geometric::{ClassLens, CoordinationLens, EdgeLengthLens, LensConfig, MassLens};
use graph_metrics::{compute_edge_strength, EdgeStrengthKind};
use std::collections::BTreeMap;

/// Logarithmic degree bands keep hubs distinct without allocating one class
/// per exact degree: 0, 1, 2-3, 4-7, 8-15, ...
fn degree_bucket(degree: usize) -> u32 {
    if degree == 0 {
        0
    } else {
        degree.ilog2() + 1
    }
}

pub fn resolve(
    lens: &LensConfig,
    snapshot: &GraphSnapshot,
) -> (GeometricSettings, GraphAttributes) {
    let mut settings = GeometricSettings::default();
    let mut attrs = GraphAttributes::default();

    // 1. Resolve Node Attributes (Class, Coordination, Mass)
    let n = snapshot.graph.nodes.len();
    let mut node_class = Vec::with_capacity(n);
    let mut node_coordination = Vec::with_capacity(n);
    let mut node_mass = Vec::with_capacity(n);

    let mut class_encoder = CategoricalEncoder::new();

    for id in &snapshot.idx_to_id {
        let node = &snapshot.graph.nodes[id];

        // Class
        let class_val = match &lens.class {
            ClassLens::Uniform => 0,
            ClassLens::DegreeBuckets => degree_bucket(node.metrics.degree),
            ClassLens::Louvain => node.metrics.community as u32,
            ClassLens::Field(f) => {
                let val = node
                    .meta
                    .frontmatter
                    .get(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                class_encoder.encode(val)
            }
            ClassLens::Tag(t) => {
                if node.meta.tags.iter().any(|tag| tag == t) {
                    1
                } else {
                    0
                }
            }
            ClassLens::NodeType => class_encoder.encode(node.meta.doctype.as_deref().unwrap_or("")),
        };
        node_class.push(class_val);

        // Coordination
        let coord_val = match &lens.coordination {
            CoordinationLens::Degree => node.metrics.degree as u32,
            CoordinationLens::Uniform(u) => *u,
            CoordinationLens::Field(f) => node
                .meta
                .frontmatter
                .get(f)
                .and_then(|v| v.as_u64())
                .map(|v| v as u32)
                .unwrap_or(0),
        };
        node_coordination.push(coord_val);

        // Mass
        let mass_val = match &lens.mass {
            MassLens::Uniform => 1.0,
            MassLens::Degree => node.metrics.degree as f32,
            MassLens::PageRank => node.metrics.pagerank as f32,
            MassLens::Field(f) => node
                .meta
                .frontmatter
                .get(f)
                .and_then(|v| v.as_f64())
                .map(|v| v as f32)
                .unwrap_or(1.0),
        };
        node_mass.push(mass_val);
    }

    // Table dimensions must cover the actual emitted class ids. Counting only
    // categorical-encoder entries made structural classes (Louvain, degree,
    // tag) fall outside the table and therefore resolve to neutral forces.
    let num_classes = node_class
        .iter()
        .copied()
        .max()
        .map_or(1, |max_class| max_class as usize + 1);
    attrs.node_class = Some(node_class);
    settings.class_source = ClassSource::Injected;

    settings.class_radius = vec![lens.exclusion_strength / 10.0; num_classes];
    settings.class_affinity_dim = num_classes as u32;
    settings.class_affinity = vec![lens.affinity_strength; num_classes * num_classes];
    for i in 0..num_classes {
        settings.class_affinity[i * num_classes + i] = 0.0;
    }

    attrs.node_coordination = Some(node_coordination);
    settings.coordination_source = CoordinationSource::Injected;

    attrs.node_mass = Some(node_mass);
    settings.mass_source = MassSource::Injected;

    // 2. Resolve Edge Attributes (Edge Length).
    //
    // For the structural-strength lenses we compute a per-edge neighbourhood
    // overlap metric (parallel to `graph.edges`) once, then map it to a spring
    // rest length — embedded intra-cluster edges short, global shortcuts long —
    // so small-world communities separate instead of collapsing into a hairball.
    // See docs/small-world-layout-research.md.
    let edge_rest_lens: Option<Vec<f32>> = match &lens.edge_length {
        EdgeLengthLens::JaccardStrength => Some(
            compute_edge_strength(&snapshot.graph, EdgeStrengthKind::Jaccard)
                .to_rest_lengths(settings.edge_rest_len, lens.edge_strength_spread),
        ),
        EdgeLengthLens::CorrectedOverlapStrength => Some(
            compute_edge_strength(&snapshot.graph, EdgeStrengthKind::CorrectedOverlap)
                .to_rest_lengths(settings.edge_rest_len, lens.edge_strength_spread),
        ),
        // VaultEdge carries no weight/type yet → uniform rest length.
        EdgeLengthLens::Uniform | EdgeLengthLens::Weight | EdgeLengthLens::EdgeType => None,
    };

    let mut adj_lens = vec![Vec::new(); n];
    for (i, edge) in snapshot.graph.edges.iter().enumerate() {
        let (Some(&src), Some(&tgt)) = (
            snapshot.id_to_idx.get(&edge.source),
            snapshot.id_to_idx.get(&edge.target),
        ) else {
            continue;
        };
        if src == tgt {
            continue;
        }

        // `edge_rest_lens` is parallel to graph.edges (index `i`); fall back to
        // the uniform rest length when no strength lens is active.
        let len = edge_rest_lens
            .as_ref()
            .map_or(settings.edge_rest_len, |v| v[i]);
        adj_lens[src as usize].push(len);
        adj_lens[tgt as usize].push(len);
    }

    let mut flat_edge_len = Vec::new();
    for bucket in adj_lens {
        flat_edge_len.extend(bucket);
    }
    attrs.edge_len = Some(flat_edge_len);
    settings.edge_length_source = EdgeLengthSource::Injected;

    // 3. Pass through shared knobs
    settings.edge_stiffness = lens.edge_stiffness;
    settings.angle_stiffness = lens.angle_stiffness;
    settings.exclusion_strength = lens.exclusion_strength;
    settings.affinity_strength = lens.affinity_strength;
    settings.gravity = lens.gravity;
    // Integrator knobs: lens serde-defaults equal the engine defaults
    // (1.0 / 0.9 / 10.0), so an old persisted lens — and the default lens —
    // resolves byte-identically to the pre-knob settings (golden master safe).
    settings.time_step = lens.time_step;
    settings.damping = lens.damping;
    settings.max_step = lens.max_step;
    if !lens.coordination_angles.is_empty() {
        settings.coordination_angles = lens.coordination_angles.clone();
    }
    if !lens.class_radius.is_empty() {
        settings.class_radius = lens.class_radius.clone();
    }
    if !lens.class_affinity.is_empty() {
        settings.class_affinity = lens.class_affinity.clone();
        settings.class_affinity_dim = (lens.class_affinity.len() as f32).sqrt() as u32;
    }

    // 4. Dynamic-bond (self-assembly) knobs. All gated on `bonding_enabled`;
    //    when it is false (the default) the engine never reads the rest, so the
    //    resolved settings remain byte-identical to the no-bonding path. The
    //    fields map 1:1 onto the geometric engine's P1–P3 dynamic-bond knobs.
    settings.bonding_enabled = lens.bonding_enabled;
    if lens.bonding_enabled {
        settings.r_bond = lens.r_bond;
        settings.r_break = lens.r_break;
        settings.bond_every = lens.bond_every;
        settings.bond_stiffness = lens.bond_stiffness;
        settings.default_max_valence = lens.default_max_valence;
        settings.default_bond_angle = lens.default_bond_angle;
        settings.line_tension = lens.line_tension;
        settings.spont_curvature = lens.spont_curvature;

        // Membrane / thermostat knobs the validated self-assembly regimes need
        // (cohesion well + Langevin temperature + patchy alignment + flattening
        // + tilt coupling). Forwarded only inside the bonding branch so the
        // no-bonding default path stays byte-identical.
        settings.well_depth = lens.well_depth;
        settings.well_width = lens.well_width;
        settings.temperature = lens.temperature;
        settings.anisotropy_strength = lens.anisotropy_strength;
        settings.gb_side_strength = lens.gb_side_strength;
        settings.tilt_coupling_strength = lens.tilt_coupling_strength;
    }

    (settings, attrs)
}

/// Encode host attributes into the proto wire form (raw LE bytes).
pub fn encode_proto(
    attrs: graph_compute::engines::GraphAttributes,
) -> graph_compute::proto::GraphAttributes {
    graph_compute::proto::GraphAttributes {
        node_class: attrs
            .node_class
            .map(|v| bytemuck::cast_slice::<u32, u8>(&v).to_vec())
            .unwrap_or_default(),
        node_coordination: attrs
            .node_coordination
            .map(|v| bytemuck::cast_slice::<u32, u8>(&v).to_vec())
            .unwrap_or_default(),
        node_mass: attrs
            .node_mass
            .map(|v| bytemuck::cast_slice::<f32, u8>(&v).to_vec())
            .unwrap_or_default(),
        edge_len: attrs
            .edge_len
            .map(|v| bytemuck::cast_slice::<f32, u8>(&v).to_vec())
            .unwrap_or_default(),
    }
}

struct CategoricalEncoder {
    map: BTreeMap<String, u32>,
}

impl CategoricalEncoder {
    fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }
    fn encode(&mut self, s: &str) -> u32 {
        let n = self.map.len() as u32;
        *self.map.entry(s.to_string()).or_insert(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_loader::{
        DiscoveryField, DiscoveryFieldType, EdgeTypeSchema, ImporterSchema, SearchDocument,
    };
    use vault_data::{NodeMetrics, VaultEdge, VaultGraph, VaultNode};

    fn test_snapshot(mut graph: VaultGraph) -> GraphSnapshot {
        let search_documents = graph
            .nodes
            .values_mut()
            .map(|node| {
                if node.meta.source_id.is_empty() {
                    node.meta.source_id = "test".into();
                }
                if node.meta.title.is_empty() {
                    node.meta.title = node.id.clone();
                }
                SearchDocument::new(&node.id)
                    .with("id", node.id.clone())
                    .with("title", node.meta.title.clone())
                    .with("tags", serde_json::json!(node.meta.tags))
            })
            .collect();
        let schema = ImporterSchema::new(
            vec![
                DiscoveryField::new("id", DiscoveryFieldType::Keyword, true).searchable(2),
                DiscoveryField::new("title", DiscoveryFieldType::Text, true).searchable(4),
                DiscoveryField::new("tags", DiscoveryFieldType::KeywordList, true).searchable(2),
            ],
            vec![EdgeTypeSchema::directed("reference", "test edge")],
        );
        GraphSnapshot::build(
            graph,
            crate::state::SnapshotSource::new("test", "Test", "1"),
            schema,
            search_documents,
        )
        .unwrap()
    }

    #[test]
    fn test_resolve_uniform() {
        let mut graph = VaultGraph::default();
        let node_id = "node1".to_string();
        graph.nodes.insert(
            node_id.clone(),
            VaultNode {
                id: node_id.clone(),
                metrics: NodeMetrics {
                    degree: 5,
                    community: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let snap = test_snapshot(graph);

        let lens = LensConfig::default();
        let (settings, attrs) = resolve(&lens, &snap);

        assert_eq!(attrs.node_class, Some(vec![0]));
        assert_eq!(attrs.node_coordination, Some(vec![0]));
        assert_eq!(attrs.node_mass, Some(vec![1.0]));
        assert_eq!(settings.class_radius.len(), 1);
    }

    /// End-to-end: the JaccardStrength edge-length lens turns per-edge structural
    /// overlap into injected `edge_len`, stretching global shortcuts while keeping
    /// embedded intra-cluster edges at the base rest length. Two triangles joined
    /// by a single bridge: triangle edges → ~base, bridge → base·(1+spread).
    #[test]
    fn jaccard_strength_lens_stretches_shortcuts() {
        let mut graph = VaultGraph::default();
        for id in ["a", "b", "c", "d", "e", "f"] {
            graph.nodes.insert(
                id.to_string(),
                VaultNode {
                    id: id.to_string(),
                    ..Default::default()
                },
            );
        }
        for (s, t) in [
            ("a", "b"),
            ("b", "c"),
            ("c", "a"),
            ("d", "e"),
            ("e", "f"),
            ("f", "d"),
            ("c", "d"), // the global shortcut
        ] {
            graph.add_edge(VaultEdge {
                source: s.to_string(),
                target: t.to_string(),
            });
        }

        let snap = test_snapshot(graph);

        let lens = LensConfig {
            edge_length: EdgeLengthLens::JaccardStrength,
            edge_strength_spread: 3.0,
            ..Default::default()
        };
        let (settings, attrs) = resolve(&lens, &snap);

        let edge_len = attrs.edge_len.expect("edge_len present");
        // 7 undirected edges → each pushed to both endpoints → 14 CSR entries.
        assert_eq!(edge_len.len(), 14);
        let min = edge_len.iter().cloned().fold(f32::MAX, f32::min);
        let max = edge_len.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            (min - 1.0).abs() < 1e-3,
            "embedded edges should sit at base rest length, got {min}"
        );
        assert!(
            (max - 4.0).abs() < 1e-3,
            "shortcut should stretch to base·(1+spread)=4.0, got {max}"
        );
        assert_eq!(settings.edge_length_source, EdgeLengthSource::Injected);
    }

    fn class_snapshot(values: &[(usize, usize, bool)]) -> GraphSnapshot {
        let mut graph = VaultGraph::default();
        for (i, &(degree, community, tagged)) in values.iter().enumerate() {
            let id = format!("n{i}");
            graph.nodes.insert(
                id.clone(),
                VaultNode {
                    id,
                    metrics: NodeMetrics {
                        degree,
                        community,
                        ..Default::default()
                    },
                    meta: vault_data::NodeMeta {
                        tags: tagged.then(|| "selected".to_string()).into_iter().collect(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        }
        test_snapshot(graph)
    }

    #[test]
    fn degree_buckets_use_degree_and_size_the_class_tables() {
        let snap = class_snapshot(&[
            (0, 99, false),
            (1, 99, false),
            (2, 99, false),
            (3, 99, false),
            (4, 99, false),
            (8, 99, false),
        ]);
        let lens = LensConfig {
            class: ClassLens::DegreeBuckets,
            ..Default::default()
        };

        let (settings, attrs) = resolve(&lens, &snap);
        assert_eq!(attrs.node_class, Some(vec![0, 1, 2, 2, 3, 4]));
        assert_eq!(settings.class_affinity_dim, 5);
        assert_eq!(settings.class_radius.len(), 5);
        assert_eq!(settings.class_affinity.len(), 25);
    }

    #[test]
    fn structural_class_tables_cover_louvain_and_tag_ids() {
        let snap = class_snapshot(&[(1, 0, false), (1, 3, true)]);

        let louvain = LensConfig {
            class: ClassLens::Louvain,
            ..Default::default()
        };
        let (settings, attrs) = resolve(&louvain, &snap);
        assert_eq!(attrs.node_class, Some(vec![0, 3]));
        assert_eq!(settings.class_affinity_dim, 4);
        assert_eq!(settings.class_affinity.len(), 16);

        let tag = LensConfig {
            class: ClassLens::Tag("selected".into()),
            ..Default::default()
        };
        let (settings, attrs) = resolve(&tag, &snap);
        assert_eq!(attrs.node_class, Some(vec![0, 1]));
        assert_eq!(settings.class_affinity_dim, 2);
        assert_eq!(settings.class_affinity.len(), 4);
    }

    fn one_node_snapshot() -> GraphSnapshot {
        let mut graph = VaultGraph::default();
        graph.nodes.insert(
            "n".to_string(),
            VaultNode {
                id: "n".to_string(),
                ..Default::default()
            },
        );
        test_snapshot(graph)
    }

    /// The default lens (bonding OFF) must leave the resolved engine settings'
    /// `bonding_enabled` false — the byte-identical default path.
    #[test]
    fn bonding_off_by_default_in_resolved_settings() {
        let (settings, _) = resolve(&LensConfig::default(), &one_node_snapshot());
        assert!(!settings.bonding_enabled);
        assert!(settings.max_valence.is_empty());
        assert_eq!(settings.default_max_valence, 0);
        assert_eq!(settings.line_tension, 0.0);
        assert_eq!(settings.spont_curvature, 0.0);
    }

    /// With `bonding_enabled`, every dynamic-bond knob is forwarded 1:1 onto the
    /// resolved `GeometricSettings` so a UI/tvix preset actually drives the
    /// self-assembly stage.
    #[test]
    fn bonding_knobs_pass_through_when_enabled() {
        let lens = LensConfig {
            bonding_enabled: true,
            r_bond: 1.1,
            r_break: 1.5,
            bond_every: 4,
            bond_stiffness: 0.4,
            default_max_valence: 3,
            default_bond_angle: 120.0,
            line_tension: 4.0,
            spont_curvature: 0.5,
            ..Default::default()
        };

        let (settings, _) = resolve(&lens, &one_node_snapshot());
        assert!(settings.bonding_enabled);
        assert_eq!(settings.r_bond, 1.1);
        assert_eq!(settings.r_break, 1.5);
        assert_eq!(settings.bond_every, 4);
        assert_eq!(settings.bond_stiffness, 0.4);
        assert_eq!(settings.default_max_valence, 3);
        assert_eq!(settings.default_bond_angle, 120.0);
        assert_eq!(settings.line_tension, 4.0);
        assert_eq!(settings.spont_curvature, 0.5);
    }
}
