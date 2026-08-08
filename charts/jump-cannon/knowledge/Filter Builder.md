---
doctype: guide
area: product
audience: [user, developer, agent]
status: current
tags: [jump-cannon, filter, search, boolean]
---

# Filter Builder

The Filter panel is a direct-manipulation Boolean outline. It describes intent
as nested sentences instead of exposing a flat predicate string:

- **Match all** keeps nodes present in every rule or child group.
- **Match any** keeps nodes present in at least one rule or child group.
- **Exclude** inverts any rule or whole group against the current graph.

Search is an ordinary repeatable rule, not a special singleton input. Field
rules use the active importer's facetable fields and value counts from
`GET /graph/meta_summary`. Search rules use the same schema-backed Tantivy
syntax as the Nodes workbench. Groups can be nested, reordered, disabled for
comparison, or moved by drag and by keyboard-accessible controls. This keeps
arbitrary AND, OR, NOT, and grouping available without asking users to reason
about operator precedence or normal forms.

Every rule and group reports its own match count. The panel validates empty
values, unavailable fields, regular expressions, and server search syntax at
the rule that owns the problem. An incomplete or invalid draft stays editable
while the graph continues to show the last valid result; fixing the draft
applies it automatically. A valid zero-result expression remains a real
zero-result expression rather than silently restoring every node.

**Filter** hides non-matching nodes and their incident edges. **Dim** keeps the
whole topology visible while reducing non-matches to context. Clear all is the
single full-reset action. Field badges in Nodes, Inspector, and Document add or
remove exact field rules in the same canonical expression.

The canonical model lives in the pure Rust `app/filter-model` crate. The UI,
AppState sharing, field badges, GPU mask, and browser regression all consume
that stable-ID expression tree. Imported v1 facet selections retain their live
behavior. Old v1 scratchpad cards are preserved as disabled draft rules because
they never affected the graph and must not become active merely by upgrading.

Indexed search and metadata facets require a server-hosted graph. Client-owned
generated graphs can still display the builder but explain why those rule types
cannot be applied. See [[Nodes Search and Documents]], [[Backend API]], and
[[Browser Regression]].
