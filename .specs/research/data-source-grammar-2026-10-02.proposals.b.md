# DataSource Grammar Proposals

**Date:** 2026-10-02  
**Project:** panel-kit-core upstream of jump-cannon importer model  
**Context:** Design 6 distinct approaches for DataSource grammar abstraction

---

## Approach 1: Direct Specification Extension

**Summary:** Extend `WorkspaceSpec` with a `data_sources` field containing `DataSourceSpec` structs that mirror the importer TOML schema.

### Description
Core adds `DataSourceSpec` (and supporting types) to `spec.rs`, serialized via serde with `#[serde(default)]` to avoid breaking existing layouts. `WorkspaceSpec` gets:

```rust
pub struct DataSourceSpec {
    pub id: String,
    pub name: String,
    pub version: String,
    pub limits: DataSourceLimits,
    pub schema: DataSourceSchema,
    // Parser details kept minimal in core
    pub parser_type: String,
}

pub struct DataSourceSchema {
    pub fields: Vec<DataSourceField>,
    pub edge_types: Vec<EdgeType>,
}
```

Shells implement `DataSourceProvider` traits to interpret `parser_type` and handle runtime transport (auth, pagination, endpoints). Core validates schema at `WorkspaceSpec` resolution time; runtime loading happens in the shell's loader.

### Key Design Decisions
- **Minimal parser in core:** Core stores `parser_type` as string; actual parser logic lives in shells.
- **`#[serde(default)]` on new fields:** Existing single-spec consumers see no change; new optional field.
- **Composable:** Multiple datasources can be declared; shells map them to panel data.

### Trade-offs
- **Pro:** Simple migration path; minimal API surface.
- **Pro:** Schema is self-describing and versioned.
- **Con:** Tightly couples data grammar to `WorkspaceSpec` lifecycle.

### Probability
0.85

### Complexity
Low

### Risks
- Adding a field to `WorkspaceSpec` requires updating all code paths that construct it manually.
- Parser type as string allows runtime mismatches if shell doesn't support it.

---

## Approach 2: Topos-Based Composition

**Summary:** Make `WorkspaceSpec` support multiple `Topos` blocks, each being either a `DataSourceSpec` or a `LayoutSpec`.

### Description
Introduce a `Topo` enum representing independent content sources:

```rust
pub enum Topo {
    DataSource(DataSourceSpec),
    WorkspaceLayout(WorkspaceSpec),
}

pub struct WorkspaceSpec {
    pub id: String,
    pub topos: Vec<Topo>,
    // ...
}
```

Each `DataSource` topological block is independent; shells resolve them to panels. `Topo` blocks are isolated in `LayoutStore` using unique storage keys.

### Key Design Decisions
- **Topological independence:** Each data source is a self-contained unit.
- **Unified persistence:** All content sources share same storage infrastructure.
- **No breaking changes:** Existing layouts are `Topo::WorkspaceLayout` single-item lists.

### Trade-offs
- **Pro:** Clean semantic model; easy to add new content types.
- **Pro:** Enables mixing multiple datasources in one workspace.
- **Con:** Adds enum indirection in resolution logic.

### Probability
0.80

### Complexity
Low

### Risks
- Existing consumers using direct `WorkspaceSpec` may need to wrap in `Topo::WorkspaceLayout`.
- Storage key generation becomes more complex.

---

## Approach 3: Plugin Registry Pattern

**Summary:** Core defines `DataSourceCatalog` trait; shells register provider implementations. DataSourceSpecs are minimal references.

### Description
Core has `DataSourceCatalog` trait for resolution and validation:

```rust
pub trait DataSourceCatalog: Send + Sync {
    fn resolve(&self, id: &str) -> Option<Arc<DataSourceDefinition>>;
    fn validate(&self, spec: &DataSourceSpec) -> Result<(), DataSourceError>;
}
```

`DataSourceSpec` becomes thin:

```rust
pub struct DataSourceSpec {
    pub id: String,
    pub config: serde_json::Value, // Opaque runtime config
}
```

Shells register implementations. Core stores `config` opaque; shell validates and interprets it.

### Key Design Decisions
- **Registry decouples core from providers:** Core doesn't know about specific data sources.
- **Opaque config:** Core stores shell-specific configuration without parsing it.
- **Trait-based:** Multiple catalogs possible for different runtime modes.

### Trade-offs
- **Pro:** Maximum modularity; can swap implementations.
- **Pro:** Core stays pure; no schema validation in core.
- **Con:** Runtime resolution complexity in shell.

### Probability
0.75

### Complexity
Medium

### Risks
- Harder to validate at workspace load time.
- Registry lifecycle needs explicit management.

---

## Approach 4: Zero-Schema Structural Model

**Summary:** Core defines only structural graph layout (SoA); no declarative schema at all.

### Description
Core owns `DataSource` as data containers:

```rust
pub struct DataSource {
    pub id: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

pub struct Node {
    pub id: String,
    pub data: Vec<(String, serde_json::Value)>, // Flexible key-value
}
```

No declarative schema. Shells define expected shapes at runtime; core just loads and stores data.

### Key Design Decisions
- **Zero-schema:** Core treats data as opaque containers.
- **Flexible structure:** Key-value pairs in nodes.
- **Shell owns schema:** Shells validate structure when using data.

### Trade-offs
- **Pro:** Maximum flexibility; no schema evolution.
- **Pro:** Simplest core model.
- **Con:** No compile-time or load-time validation.
- **Con:** Runtime errors from mismatched expectations.

### Probability
0.45

### Complexity
Low

### Risks
- Debugging runtime errors is harder without schema.
- Tooling (autocomplete, IDE support) suffers.

---

## Approach 5: DSL-Based Declaration

**Summary:** Core defines a mini-DSL (string tokens); DSL parsed at runtime into typed schema.

### Description
Core stores DataSource declarations as strings:

```rust
pub struct DataSourceSpec {
    pub id: String,
    pub definition: String, // e.g. "field body:text searchable; edge mentions -> entity;"
}
```

Shell implements DSL parser. Core stores raw string; shell validates and interprets.

### Key Design Decisions
- **DSL format:** Human-readable string tokens.
- **Shell parses:** No schema validation in core.
- **Versioning:** Include format version in string prefix.

### Trade-offs
- **Pro:** Human-editable; compact representation.
- **Pro:** Extensible via new DSL rules.
- **Con:** DSL parsing adds complexity to shells.
- **Con:** Error messages depend on parser quality.

### Probability
0.35

### Complexity
Medium

### Risks
- DSL parsing errors at runtime.
- DSL evolution requires versioning strategy.

---

## Approach 6: Schema Validation First

**Summary:** Core validates DataSource schema at load time using JSON Schema; strict validation before data loading.

### Description
Core includes JSON Schema validation (via `schemars` and `jsonschema`):

```rust
pub struct DataSourceSpec {
    pub id: String,
    pub schema: JsonSchemaValue, // RFC8259 JSON Schema
    pub config: JsonValue,
}
```

`JsonSchemaValue` validates `config` at workspace resolution. Core rejects invalid schemas; shells only see validated configs.

### Key Design Decisions
- **Strict validation:** Core enforces schema validity.
- **JSON Schema standard:** Leverages existing tools.
- **Early failure:** Catch errors at workspace load, not runtime.

### Trade-offs
- **Pro:** High confidence in data validity.
- **Pro:** Standard format; tooling support.
- **Con:** Adds `jsonschema` dependency to core.
- **Con:** Larger bundle size.

### Probability
0.30

### Complexity
Medium

### Risks
- Dependency increases core complexity.
- Schema validation may be slower than simple serde.

---

## Recommendation

For practical integration with panel-kit-core's existing architecture, **Approach 1 (Direct Specification Extension)** offers the best balance:

- Minimal changes to `WorkspaceSpec`
- Existing single-spec consumers unaffected  
- Schema self-describing and versioned
- Parser implementation stays in shells

Use `#[serde(default)]` for the new field to ensure forward compatibility. The remaining approaches provide exploration paths for future extensibility.
