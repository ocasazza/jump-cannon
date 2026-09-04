# Hindsight memory-bank package, authored in Nix.
#
# Same package as `hindsight-memory-bank.toml`: graph-api evaluates this file
# with the in-tree tvix evaluator (`builtins.toJSON` of the attrset) and feeds
# the JSON through the identical HttpJsonManifest validation. Nix is a second
# serialization of the same package format — no new engine, no new SourceKind.
# The `let` bindings below are why: repeated field/schema declarations become
# one-line calls instead of copied TOML tables.
#
# Verified against the Hindsight 0.9.1 API deployed behind
# `http://hindsight-api-proxy.hindsight.svc.cluster.local`:
#   GET /v1/{tenant}/banks                                -> {"banks":[{"bank_id":...}]}
#   GET /v1/{tenant}/banks/{bank}/memories/list?state=…   -> {"items":[…],"total":N}
#   GET /v1/{tenant}/banks/{bank}/graph?limit=            -> {"edges":[{"data":{...}}],"total_units":N}
#   GET /v1/{tenant}/banks/{bank}/entities?limit=&offset= -> {"items":[…],"total":N}
#   GET /v1/{tenant}/banks/{bank}/documents?limit=&offset=-> {"items":[…],"total":N}
let
  bank = "/v1/{tenant}/banks/{bank}";
  limitOffset = { style = "limit_offset"; };

  # One `[[collections.nodes.fields]]` entry: package field key <- JSON pointer.
  field = key: pointer: { inherit key pointer; };
  csvField = key: pointer: { inherit key pointer; transform = "split_csv"; };

  # One `[[schema.fields]]` entry. Every package field is optional and
  # searchable; `facetable`/`snippet`/`boost` vary per field.
  schemaField = key: field_type: extra: {
    inherit key field_type;
    required = false;
    searchable = true;
    facetable = false;
  } // extra;
  text = key: extra: schemaField key "text" extra;
  keyword = key: extra: schemaField key "keyword" extra;
  number = key: schemaField key "number" { facetable = true; };

  edgeType = key: directed: description: { inherit key directed description; };
in
{
  format_version = 1;

  metadata = {
    id = "hindsight.memory-bank";
    name = "Hindsight memory bank";
    version = "1.0.0";
    description = "Reads one bank of a Hindsight memory service as a read-only graph: valid memory units, canonical entities, retained documents, plus the unit->unit temporal/semantic/caused_by links Hindsight's own graph serves and the unit->entity/unit->document edges derived from the unit record.";
  };

  # Bound by an administrator at instance time, never baked in.
  variables = [
    {
      name = "tenant";
      default = "default";
      description = "Tenant path segment: /v1/{tenant}/banks/...";
    }
    {
      name = "bank";
      description = "The memory bank to import. Must exist in /v1/{tenant}/banks; a mistyped bank fails the import loudly with the banks that do.";
    }
  ];

  # Existence check on the bound bank.
  preflight = {
    path = "/v1/{tenant}/banks";
    items_pointer = "/banks";
    id_pointer = "/bank_id";
    variable = "bank";
    subject = "bank";
  };

  collections = [
    # memories: valid memory units (state=valid). The state filter is the
    # API-side `query`, repeated client-side in `skip_unless` so an older API
    # ignoring `state=` still drops invalidated units. `fact_type` (world |
    # experience | observation) becomes the per-node doctype and stays indexed
    # raw for exact-value search.
    {
      name = "memories";
      path = "${bank}/memories/list";
      query = { state = "valid"; };
      paginate = limitOffset;
      nodes = {
        id_pointer = "/id";
        node_type = "memory";
        tags_pointer = "/tags";
        skip_unless = { pointer = "/state"; equals = "valid"; };
        doctype = {
          pointer = "/fact_type";
          map = {
            world = "World Fact";
            experience = "Experience";
            observation = "Observation";
          };
        };
        title = { pointer = "/text"; fallback_prefix = "memory"; };
        fields = [
          (field "body" "/text")
          (field "context" "/context")
          (csvField "entities" "/entities")
          (field "fact_type" "/fact_type")
          (field "state" "/state")
          (field "document_id" "/document_id")
          (field "session_id" "/metadata/session_id")
          (field "mentioned_at" "/mentioned_at")
          (field "proof_count" "/proof_count")
        ];
        edges = [
          # mentions: each unit -> each canonical entity named in /entities.
          {
            kind = "mentions";
            value_pointer = "/entities";
            transform = "split_csv";
            target_collection = "entities";
            match_on = "title";
          }
          # documented_in: each unit -> the source document retained against.
          {
            kind = "documented_in";
            value_pointer = "/document_id";
            target_collection = "documents";
            match_on = "id";
          }
        ];
      };
    }

    # entities: canonical entities Hindsight has surfaced across the bank.
    {
      name = "entities";
      path = "${bank}/entities";
      paginate = limitOffset;
      nodes = {
        id_pointer = "/id";
        local_prefix = "entity:";
        node_type = "entity";
        title = { pointer = "/canonical_name"; fallback_prefix = "entity"; };
        fields = [ (field "mention_count" "/mention_count") ];
      };
    }

    # documents: retained documents the units were consolidated from. Title is
    # the session id when present (sessions form a facet) else the document id.
    {
      name = "documents";
      path = "${bank}/documents";
      paginate = limitOffset;
      nodes = {
        id_pointer = "/id";
        local_prefix = "document:";
        node_type = "document";
        tags_pointer = "/tags";
        title = { pointer = "/retain_params/metadata/session_id"; fallback_prefix = "document"; };
        fields = [
          (field "memory_count" "/memory_unit_count")
          (field "session_id" "/retain_params/metadata/session_id")
          (field "body" "/retain_params/context")
        ];
      };
    }

    # links: unit->unit edges from Hindsight's own memory graph. The `entity`
    # link kind is dropped; the bipartite `mentions` edges above express it.
    {
      name = "links";
      path = "${bank}/graph";
      items_pointer = "/edges";
      total_pointer = "/total_units";
      edges = {
        source_pointer = "/data/source";
        target_pointer = "/data/target";
        kind_pointer = "/data/linkType";
        include_kinds = [ "temporal" "semantic" "caused_by" ];
        endpoints_collection = "memories";
        drop_self_loops = true;
        dedupe = "unordered";
      };
    }
  ];

  # Discovery schema: `id`, `title`, `tags`, `path`, `type`, `folder` are
  # supplied by the engine; each entry here names one `field` key above.
  schema = {
    fields = [
      (text "body" { snippet = true; boost = 4; })
      (text "context" { boost = 2; })
      (schemaField "entities" "keyword_list" { facetable = true; boost = 2; })
      (keyword "fact_type" { facetable = true; })
      (keyword "state" { facetable = true; })
      (keyword "document_id" { boost = 2; })
      (keyword "session_id" { facetable = true; boost = 2; })
      (schemaField "mentioned_at" "date" { facetable = true; })
      (number "proof_count")
      (number "mention_count")
      (number "memory_count")
    ];
    edge_types = [
      (edgeType "temporal" true "Hindsight unit->unit temporal ordering.")
      (edgeType "semantic" false "Hindsight unit<->unit semantic similarity (undirected).")
      (edgeType "caused_by" true "Hindsight unit->unit causal relation.")
      (edgeType "mentions" true "Memory unit -> canonical entity named in its /entities (CSV split).")
      (edgeType "documented_in" true "Memory unit -> retained document it was consolidated from.")
    ];
  };

  # `request_timeout_seconds = 120` covers the ~24s observed for the largest
  # graph query with room for retries; `max_records` is the loud hard bound.
  limits = {
    request_timeout_seconds = 120;
    max_records = 50000;
  };
}
