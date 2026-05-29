use std::collections::BTreeMap;
use graph_compute::engines::geometric::{
    ClassSource, CoordinationSource, EdgeLengthSource, GeometricSettings, MassSource,
};
use graph_compute::engines::GraphAttributes;
use graph_layouts::geometric::{
    ClassLens, CoordinationLens, EdgeLengthLens, LensConfig, MassLens,
};
use crate::state::GraphSnapshot;

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
            ClassLens::DegreeBuckets => {
                node.metrics.community as u32
            }
            ClassLens::Louvain => node.metrics.community as u32,
            ClassLens::Field(f) => {
                let val = node.meta.frontmatter.get(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                class_encoder.encode(val)
            }
            ClassLens::Tag(t) => {
                if node.meta.tags.iter().any(|tag| tag == t) { 1 } else { 0 }
            }
            ClassLens::NodeType => {
                class_encoder.encode(node.meta.doctype.as_deref().unwrap_or(""))
            }
        };
        node_class.push(class_val);

        // Coordination
        let coord_val = match &lens.coordination {
            CoordinationLens::Degree => node.metrics.degree as u32,
            CoordinationLens::Uniform(u) => *u,
            CoordinationLens::Field(f) => {
                node.meta.frontmatter.get(f)
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(0)
            }
        };
        node_coordination.push(coord_val);

        // Mass
        let mass_val = match &lens.mass {
            MassLens::Uniform => 1.0,
            MassLens::Degree => node.metrics.degree as f32,
            MassLens::PageRank => node.metrics.pagerank as f32,
            MassLens::Field(f) => {
                node.meta.frontmatter.get(f)
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32)
                    .unwrap_or(1.0)
            }
        };
        node_mass.push(mass_val);
    }

    attrs.node_class = Some(node_class);
    settings.class_source = ClassSource::Injected;
    
    let num_classes = class_encoder.map.len().max(1);
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

    // 2. Resolve Edge Attributes (Edge Length)
    let mut adj_lens = vec![Vec::new(); n];
    for edge in &snapshot.graph.edges {
        let (Some(&src), Some(&tgt)) = (snapshot.id_to_idx.get(&edge.source), snapshot.id_to_idx.get(&edge.target)) else { continue };
        if src == tgt { continue }
        
        let len = match &lens.edge_length {
            EdgeLengthLens::Uniform => 1.0,
            _ => 1.0, // VaultEdge has no weight/type yet
        };
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

    (settings, attrs)
}

/// Encode host attributes into the proto wire form (raw LE bytes).
pub fn encode_proto(
    attrs: graph_compute::engines::GraphAttributes,
) -> graph_compute::proto::GraphAttributes {
    graph_compute::proto::GraphAttributes {
        node_class: attrs.node_class
            .map(|v| bytemuck::cast_slice::<u32, u8>(&v).to_vec())
            .unwrap_or_default(),
        node_coordination: attrs.node_coordination
            .map(|v| bytemuck::cast_slice::<u32, u8>(&v).to_vec())
            .unwrap_or_default(),
        node_mass: attrs.node_mass
            .map(|v| bytemuck::cast_slice::<f32, u8>(&v).to_vec())
            .unwrap_or_default(),
        edge_len: attrs.edge_len
            .map(|v| bytemuck::cast_slice::<f32, u8>(&v).to_vec())
            .unwrap_or_default(),
    }
}

struct CategoricalEncoder {
    map: BTreeMap<String, u32>,
}

impl CategoricalEncoder {
    fn new() -> Self {
        Self { map: BTreeMap::new() }
    }
    fn encode(&mut self, s: &str) -> u32 {
        let n = self.map.len() as u32;
        *self.map.entry(s.to_string()).or_insert(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vault_data::{NodeMeta, NodeMetrics, VaultNode, VaultGraph};
    use std::collections::HashMap;

    #[test]
    fn test_resolve_uniform() {
        let mut graph = VaultGraph::default();
        let node_id = "node1".to_string();
        graph.nodes.insert(node_id.clone(), VaultNode {
            id: node_id.clone(),
            metrics: NodeMetrics { degree: 5, community: 1, ..Default::default() },
            ..Default::default()
        });
        
        let snap = GraphSnapshot {
            graph,
            id_to_idx: [("node1".to_string(), 0)].into_iter().collect(),
            idx_to_id: vec!["node1".to_string()],
            binary_cache: HashMap::new(),
        };

        let lens = LensConfig::default();
        let (settings, attrs) = resolve(&lens, &snap);

        assert_eq!(attrs.node_class, Some(vec![0]));
        assert_eq!(attrs.node_coordination, Some(vec![0]));
        assert_eq!(attrs.node_mass, Some(vec![1.0]));
        assert_eq!(settings.class_radius.len(), 1);
    }
}
