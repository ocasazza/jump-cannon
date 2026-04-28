use super::combinators::Op;

pub type NodeId = String;

/// A mock graph for testing: just a list of node ids with metadata.
#[derive(Clone, Debug)]
pub struct MockGraph {
    pub nodes: Vec<NodeId>,
}

pub fn evaluate(pipeline: &[Op], graph: &MockGraph) -> Result<Vec<NodeId>, String> {
    let mut stream: Vec<NodeId> = graph.nodes.clone();
    for op in pipeline {
        stream = apply_op(op, stream, graph)?;
    }
    Ok(stream)
}

fn apply_op(op: &Op, nodes: Vec<NodeId>, _graph: &MockGraph) -> Result<Vec<NodeId>, String> {
    match op {
        Op::Passthrough => Ok(nodes),
        Op::Top(n) => Ok(nodes.into_iter().take(*n).collect()),
        Op::Filter { field, value } => {
            // Mock: keep nodes whose id contains value if field=="id", else passthrough
            if field == "id" {
                Ok(nodes.into_iter().filter(|n| n.contains(value.as_str())).collect())
            } else {
                Ok(nodes) // future: real metadata filtering
            }
        }
        Op::Map { field: _ } => Ok(nodes), // future: project field
        Op::Sort { desc, .. } => {
            let mut out = nodes;
            out.sort();
            if *desc { out.reverse(); }
            Ok(out)
        }
        Op::SelfJoin => {
            let doubled = nodes.iter().cloned().chain(nodes.iter().cloned()).collect();
            Ok(doubled)
        }
        Op::Neighbors { depth } => {
            // Mock: return nodes with "-neighbor" appended, up to depth times
            let mut out = nodes.clone();
            for _ in 0..*depth {
                let neighbors: Vec<_> = out.iter().map(|n| format!("{}-neighbor", n)).collect();
                out.extend(neighbors);
            }
            Ok(out)
        }
        Op::Recurse { max_depth, .. } => {
            // Mock: apply Op::Neighbors once per depth step
            let mut out = nodes;
            for _ in 0..*max_depth {
                let prev_len = out.len();
                let new_nodes: Vec<_> = out.iter().map(|n| format!("{}-r", n)).collect();
                out.extend(new_nodes);
                if out.len() == prev_len { break; } // fixed point
                if out.len() > 1000 { break; } // safety cap
            }
            Ok(out)
        }
        Op::Nix(expr) => {
            // Delegate to tvix-wasm native eval
            tvix_wasm::eval_nix(expr)
                .map(|result| vec![result])
                .map_err(|e| format!("nix eval error: {}", e))
        }
        _ => Ok(nodes), // remaining ops: passthrough for now
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parser::parse;

    fn mock() -> MockGraph {
        MockGraph { nodes: vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()] }
    }

    #[test] fn test_passthrough() { assert_eq!(evaluate(&parse("passthrough").unwrap(), &mock()).unwrap().len(), 5); }
    #[test] fn test_top() { assert_eq!(evaluate(&parse("top 3").unwrap(), &mock()).unwrap().len(), 3); }
    #[test] fn test_pipe() { assert_eq!(evaluate(&parse("passthrough | top 2").unwrap(), &mock()).unwrap().len(), 2); }
    #[test] fn test_sort() { let r = evaluate(&parse("sort field=id").unwrap(), &mock()).unwrap(); assert_eq!(r[0], "a"); }
    #[test] fn test_sort_desc() { let r = evaluate(&parse("sort field=id desc").unwrap(), &mock()).unwrap(); assert_eq!(r[0], "e"); }
    #[test] fn test_filter() { let r = evaluate(&parse("filter field=id value=a").unwrap(), &mock()).unwrap(); assert_eq!(r, vec!["a"]); }
    #[test] fn test_nix() { let r = evaluate(&parse("nix { 1 + 1 }").unwrap(), &mock()).unwrap(); assert_eq!(r, vec!["2"]); }
    #[test] fn test_neighbors() { let r = evaluate(&parse("neighbors depth=1").unwrap(), &mock()).unwrap(); assert!(r.len() > 5); }
}
