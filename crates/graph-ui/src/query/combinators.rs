/// A single pipeline stage, corresponding to a Smullyan combinator.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // B — pipe/compose (the | operator itself; not a named stage)
    // I — identity
    Passthrough,
    // K — keep/filter
    Filter { field: String, value: String },
    // KI — drop first / return second
    Second,
    // S — apply f and g to same input, combine
    Apply { left: Box<Op>, right: Box<Op> },
    // B1/B2/B3 — multi-stage composition (parsed as nested pipes)
    Compose3(Box<Op>, Box<Op>, Box<Op>),
    // C — flip argument order
    Flip(Box<Op>),
    // W — self-join (cross-join set with itself)
    SelfJoin,
    // T — apply-to / pipe value into function
    Into { op: Box<Op> },
    // M — apply twice
    Twice(Box<Op>),
    // Y — fixed-point / recurse until stable
    Recurse { op: Box<Op>, max_depth: usize },
    // L — partial recurse (one unrolling of Y)
    PartialRecurse(Box<Op>),
    // O — chain-into
    ChainInto { f: Box<Op>, g: Box<Op> },
    // Q — reverse composition
    ComposeRev { f: Box<Op>, g: Box<Op> },
    // R — rotate three args
    Rotate(Box<Op>),
    // V — pair/zip two streams
    Zip { right: Vec<String> },
    // Φ (S') — parallel: apply g and h, combine with f
    Parallel { combine: String, left: Box<Op>, right: Box<Op> },
    // Ψ — on: apply same transform to both sides
    On { transform: Box<Op> },
    // Γ — fold step
    FoldStep { op: Box<Op> },
    // Graph-specific
    Map { field: String },
    Sort { field: String, desc: bool },
    Top(usize),
    Stats { func: String, field: String },
    Dedup { field: String },
    Neighbors { depth: usize },
    Community,
    Centrality { metric: String },
    // .nix escape hatch — evaluated by tvix-wasm
    Nix(String),
}
