use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, INDEXED, STORED, STRING};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{
    doc, Index, IndexReader, IndexWriter, ReloadPolicy, SnippetGenerator, TantivyDocument,
    Term,
};

use crate::walker::{list_markdown, parse_note, Note};

/// Tantivy index field handles, kept together so handlers don't have to
/// re-resolve them on every request.
#[derive(Clone)]
pub struct Fields {
    pub id: Field,
    pub title: Field,
    pub body: Field,
    pub tags: Field,
    pub mtime: Field,
}

/// State the HTTP server hands out to each request handler. Cheap to clone
/// (everything inside is `Arc` or already-Clone).
#[derive(Clone)]
pub struct IndexState {
    pub vault: PathBuf,
    pub include_hippo: bool,
    pub index: Index,
    pub reader: IndexReader,
    pub fields: Fields,
    pub writer: Arc<RwLock<IndexWriter>>,
    pub query_parser: Arc<QueryParser>,
    pub indexed: Arc<AtomicUsize>,
    pub total: Arc<AtomicUsize>,
    pub status: Arc<RwLock<IndexStatus>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexStatus {
    Indexing,
    Ready,
}

impl IndexStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            IndexStatus::Indexing => "indexing",
            IndexStatus::Ready => "ready",
        }
    }
}

pub fn build_schema() -> (Schema, Fields) {
    let mut sb = Schema::builder();
    let id = sb.add_text_field("id", STRING | STORED);

    // Title gets `en_stem` for stemming and is stored for snippet/title return.
    let text_indexing = TextFieldIndexing::default()
        .set_tokenizer("en_stem")
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let text_opts = TextOptions::default()
        .set_indexing_options(text_indexing.clone())
        .set_stored();

    let title = sb.add_text_field("title", text_opts.clone());
    let body = sb.add_text_field(
        "body",
        TextOptions::default()
            .set_indexing_options(text_indexing.clone())
            .set_stored(),
    );
    let tags = sb.add_text_field("tags", text_opts.clone());
    let mtime = sb.add_u64_field("mtime", STORED | INDEXED | FAST);
    let schema = sb.build();
    (
        schema,
        Fields {
            id,
            title,
            body,
            tags,
            mtime,
        },
    )
}

/// Open or create the index at `dir`. Returns the handle plus the schema's
/// field IDs so the rest of the system doesn't have to re-resolve them.
pub fn open_or_create(dir: &Path) -> Result<(Index, Fields)> {
    std::fs::create_dir_all(dir).with_context(|| format!("create cache dir {dir:?}"))?;
    let (schema, fields) = build_schema();
    let index = if Index::exists(&tantivy::directory::MmapDirectory::open(dir)?)? {
        Index::open_in_dir(dir).context("open existing index")?
    } else {
        Index::create_in_dir(dir, schema).context("create new index")?
    };
    // Register `en_stem` analyzer (Tantivy ships it but we can also use the
    // default registry which already has it).
    let _: TextAnalyzer = index
        .tokenizers()
        .get("en_stem")
        .context("en_stem tokenizer missing")?;
    Ok((index, fields))
}

/// Pull stored mtime values out of the existing index and bucket them by id.
/// Used for incremental reindex on warm starts.
pub fn read_existing_mtimes(index: &Index, fields: &Fields) -> Result<HashMap<String, u64>> {
    let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
    let searcher = reader.searcher();
    let mut out = HashMap::new();
    for segment_reader in searcher.segment_readers() {
        let store = segment_reader.get_store_reader(64)?;
        for doc_id in 0..segment_reader.max_doc() {
            if segment_reader.is_deleted(doc_id) {
                continue;
            }
            let doc: TantivyDocument = match store.get(doc_id) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let id_val = doc
                .get_first(fields.id)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mtime = doc.get_first(fields.mtime).and_then(|v| v.as_u64());
            if let (Some(id), Some(m)) = (id_val, mtime) {
                out.insert(id, m);
            }
        }
    }
    Ok(out)
}

/// Build a fresh index from scratch (called when --rebuild or there's no
/// usable cache). Caller is responsible for `commit()`.
pub fn full_index(
    writer: &mut IndexWriter,
    fields: &Fields,
    vault: &Path,
    include_hippo: bool,
    counter: &AtomicUsize,
    total: &AtomicUsize,
) -> Result<usize> {
    writer.delete_all_documents()?;
    let files = list_markdown(vault, include_hippo)?;
    total.store(files.len(), Ordering::SeqCst);
    let mut count = 0;
    for (id, path, mtime) in files {
        match parse_note(&id, &path, mtime) {
            Ok(note) => {
                add_doc(writer, fields, &note);
                count += 1;
                counter.store(count, Ordering::SeqCst);
            }
            Err(e) => tracing::warn!(?path, error = %e, "skip note"),
        }
    }
    Ok(count)
}

/// Incremental reindex: re-parse files newer than the stored mtime, drop
/// indexed-but-deleted ids, leave everything else alone.
pub fn incremental_index(
    writer: &mut IndexWriter,
    fields: &Fields,
    existing: &HashMap<String, u64>,
    vault: &Path,
    include_hippo: bool,
    counter: &AtomicUsize,
    total: &AtomicUsize,
) -> Result<(usize, usize, usize)> {
    let files = list_markdown(vault, include_hippo)?;
    let on_disk: HashMap<&str, (&PathBuf, u64)> = files
        .iter()
        .map(|(id, p, m)| (id.as_str(), (p, *m)))
        .collect();

    total.store(files.len(), Ordering::SeqCst);

    let mut updated = 0;
    let mut added = 0;
    let mut removed = 0;

    // Drop notes that vanished on disk.
    for id in existing.keys() {
        if !on_disk.contains_key(id.as_str()) {
            writer.delete_term(Term::from_field_text(fields.id, id));
            removed += 1;
        }
    }

    for (id, path, mtime) in &files {
        let needs_index = match existing.get(id) {
            None => {
                added += 1;
                true
            }
            Some(prev) if *prev < *mtime => {
                writer.delete_term(Term::from_field_text(fields.id, id));
                updated += 1;
                true
            }
            _ => false,
        };
        if needs_index {
            match parse_note(id, path, *mtime) {
                Ok(note) => add_doc(writer, fields, &note),
                Err(e) => tracing::warn!(?path, error = %e, "skip note"),
            }
        }
        counter.fetch_add(1, Ordering::SeqCst);
    }

    Ok((added, updated, removed))
}

fn add_doc(writer: &IndexWriter, fields: &Fields, note: &Note) {
    let mut td = doc!(
        fields.id => note.id.clone(),
        fields.title => note.title.clone(),
        fields.body => note.body.clone(),
        fields.mtime => note.mtime,
    );
    for t in &note.tags {
        td.add_text(fields.tags, t);
    }
    let _ = writer.add_document(td);
}

/// Construct the shared QueryParser: parses across body/title/tags with
/// title boosted x3 and tags x2 so a hit in the title outranks a body
/// match. Lenient by default so callers can pass raw user strings without
/// quoting concerns.
pub fn build_query_parser(index: &Index, fields: &Fields) -> QueryParser {
    let mut qp = QueryParser::for_index(index, vec![fields.body, fields.title, fields.tags]);
    qp.set_field_boost(fields.title, 3.0);
    qp.set_field_boost(fields.tags, 2.0);
    qp.set_conjunction_by_default();
    qp
}

/// Search helper used by both /search and /ids. Returns up to `limit`
/// (id, score, optional snippet) tuples, scored by BM25.
pub fn run_search(
    state: &IndexState,
    q: &str,
    limit: usize,
    offset: usize,
    want_snippet: bool,
) -> Result<(usize, Vec<(String, f32, Option<String>)>)> {
    let query = state
        .query_parser
        .parse_query(q)
        .with_context(|| format!("parse query: {q}"))?;

    let searcher = state.reader.searcher();
    let collector = TopDocs::with_limit(limit + offset);
    let top = searcher.search(&query, &collector)?;

    let snippet_gen = if want_snippet {
        Some(SnippetGenerator::create(&searcher, &*query, state.fields.body)?)
    } else {
        None
    };

    let total = top.len();
    let mut out = Vec::with_capacity(limit.min(total));
    for (score, addr) in top.into_iter().skip(offset).take(limit) {
        let doc: TantivyDocument = searcher.doc(addr)?;
        let Some(id) = doc.get_first(state.fields.id).and_then(|v| v.as_str()) else {
            continue;
        };
        let snippet = snippet_gen.as_ref().map(|g| {
            let s = g.snippet_from_doc(&doc);
            s.to_html()
        });
        out.push((id.to_string(), score, snippet));
    }
    Ok((total, out))
}

/// Look up a single doc by id. None if not found.
pub fn lookup_node(state: &IndexState, id: &str) -> Result<Option<TantivyDocument>> {
    let searcher = state.reader.searcher();
    let term = Term::from_field_text(state.fields.id, id);
    let q = tantivy::query::TermQuery::new(term, IndexRecordOption::Basic);
    let top = searcher.search(&q, &TopDocs::with_limit(1))?;
    if let Some((_, addr)) = top.into_iter().next() {
        let doc: TantivyDocument = searcher.doc(addr)?;
        Ok(Some(doc))
    } else {
        Ok(None)
    }
}
