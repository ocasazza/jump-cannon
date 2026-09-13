//! Envelope tests build every archive in memory — no fixture files, no I/O.

use std::io::Write;

use data_loader::{NoProgress, Transport, WriteRequest};

use super::*;

fn record(origin: &str, bytes: Vec<u8>) -> SourceRecord {
    SourceRecord {
        origin: origin.to_string(),
        content_type: "application/octet-stream".to_string(),
        bytes,
        metadata: BTreeMap::new(),
    }
}

fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *body).unwrap();
    }
    builder.into_inner().unwrap()
}
/// Build one ustar entry by hand. `tar::Builder` refuses traversal paths,
/// which is exactly what this fixture needs to exercise.
fn raw_tar_entry(path: &str, body: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..path.len()].copy_from_slice(path.as_bytes());
    let octal = |field: &mut [u8], value: u64| {
        let text = format!("{:0>width$o}", value, width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
    };
    octal(&mut header[100..108], 0o644); // mode
    octal(&mut header[108..116], 0); // uid
    octal(&mut header[116..124], 0); // gid
    octal(&mut header[124..136], body.len() as u64); // size
    octal(&mut header[136..148], 0); // mtime
    header[156] = b'0'; // regular file
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    for byte in &mut header[148..156] {
        *byte = b' ';
    }
    let checksum: u64 = header.iter().map(|byte| *byte as u64).sum();
    let text = format!("{checksum:06o}\0 ");
    header[148..156].copy_from_slice(text.as_bytes());
    let mut out = header.to_vec();
    out.extend_from_slice(body);
    out.resize(out.len() + (512 - body.len() % 512) % 512, 0);
    out
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (path, body) in entries {
        writer.start_file(*path, options).unwrap();
        writer.write_all(body).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn gzip_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn entry_paths(records: &[SourceRecord]) -> Vec<&str> {
    records
        .iter()
        .map(|record| {
            record
                .metadata
                .get(ENTRY_KEY)
                .and_then(|value| value.as_str())
                .unwrap()
        })
        .collect()
}

#[test]
fn plain_input_passes_through_unchanged() {
    let input = record("https://x/notes.md", b"# hello".to_vec());
    let out = expand_envelope(input.clone(), &EnvelopeLimits::default()).unwrap();
    assert_eq!(out, vec![input]);
}

#[test]
fn tar_expands_to_one_record_per_file() {
    let tar = tar_bytes(&[("a/one.txt", b"one"), ("two.json", b"{}")]);
    let out = expand_envelope(record("s3://bucket/backup.tar", tar), &EnvelopeLimits::default())
        .unwrap();
    assert_eq!(entry_paths(&out), vec!["a/one.txt", "two.json"]);
    assert_eq!(out[0].origin, "s3://bucket/backup.tar!a/one.txt");
    assert_eq!(out[0].bytes, b"one");
    assert_eq!(out[0].content_type, "text/plain");
    assert_eq!(out[1].content_type, "application/json");
    assert_eq!(
        out[1].metadata.get(CONTAINER_KEY).and_then(|v| v.as_str()),
        Some("s3://bucket/backup.tar")
    );
}

#[test]
fn zip_expands_and_skips_directories() {
    let zip = zip_bytes(&[("dir/", b""), ("dir/file.txt", b"body")]);
    let out = expand_envelope(record("mem://a.zip", zip), &EnvelopeLimits::default()).unwrap();
    assert_eq!(entry_paths(&out), vec!["dir/file.txt"]);
    assert_eq!(out[0].bytes, b"body");
}

#[test]
fn gzip_single_file_strips_suffix() {
    let gz = gzip_bytes(b"raw payload");
    let out = expand_envelope(record("https://x/data.json.gz", gz), &EnvelopeLimits::default())
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(entry_paths(&out), vec!["data.json"]);
    assert_eq!(out[0].origin, "https://x/data.json.gz!data.json");
    assert_eq!(out[0].content_type, "application/json");
    assert_eq!(out[0].bytes, b"raw payload");
}

#[test]
fn tar_gz_expands_through_both_layers() {
    let tgz = gzip_bytes(&tar_bytes(&[("logs/app.log", b"log line")]));
    let out = expand_envelope(record("https://x/logs.tar.gz", tgz), &EnvelopeLimits::default())
        .unwrap();
    assert_eq!(entry_paths(&out), vec!["logs/app.log"]);
    assert_eq!(out[0].origin, "https://x/logs.tar.gz!logs/app.log");
    assert_eq!(out[0].bytes, b"log line");
}

#[test]
fn nested_archive_chains_entry_paths() {
    let inner = zip_bytes(&[("notes.md", b"notes")]);
    let outer = tar_bytes(&[("docs/inner.zip", &inner)]);
    let out = expand_envelope(record("mem://outer.tar", outer), &EnvelopeLimits::default())
        .unwrap();
    assert_eq!(entry_paths(&out), vec!["docs/inner.zip!notes.md"]);
    assert_eq!(out[0].origin, "mem://outer.tar!docs/inner.zip!notes.md");
    assert_eq!(out[0].bytes, b"notes");
}

#[test]
fn entry_count_cap_errors() {
    let tar = tar_bytes(&[("a.txt", b"a"), ("b.txt", b"b")]);
    let limits = EnvelopeLimits {
        max_entries: 1,
        ..EnvelopeLimits::default()
    };
    let error = expand_envelope(record("mem://x.tar", tar), &limits).unwrap_err();
    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("entry-count bound"));
}

#[test]
fn per_entry_byte_cap_errors_from_header() {
    let tar = tar_bytes(&[("big.bin", &[0u8; 64])]);
    let limits = EnvelopeLimits {
        max_entry_bytes: 16,
        ..EnvelopeLimits::default()
    };
    let error = expand_envelope(record("mem://x.tar", tar), &limits).unwrap_err();
    assert!(error.to_string().contains("byte bound"));
}

#[test]
fn total_byte_cap_errors_across_entries() {
    let tar = tar_bytes(&[("a.txt", &[1u8; 32]), ("b.txt", &[2u8; 32])]);
    let limits = EnvelopeLimits {
        max_total_bytes: 40,
        ..EnvelopeLimits::default()
    };
    let error = expand_envelope(record("mem://x.tar", tar), &limits).unwrap_err();
    assert!(error.to_string().contains("total-byte bound"));
}

#[test]
fn tar_traversal_entries_are_skipped() {
    let mut tar = raw_tar_entry("../evil.txt", b"x");
    tar.extend_from_slice(&raw_tar_entry("ok.txt", b"ok"));
    tar.resize(tar.len() + 1024, 0);
    let out = expand_envelope(record("mem://x.tar", tar), &EnvelopeLimits::default()).unwrap();
    assert_eq!(entry_paths(&out), vec!["ok.txt"]);
}

#[test]
fn strip_gz_suffix_handles_paths() {
    assert_eq!(strip_gz_suffix("dir/file.json.gz"), "dir/file.json");
    assert_eq!(strip_gz_suffix("file.gz"), "file");
    assert_eq!(strip_gz_suffix("file"), "file");
}

struct FixtureConnector {
    records: Vec<SourceRecord>,
}

impl SourceConnector for FixtureConnector {
    fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        vec![Capability::new(effect, Transport::InMemory, "fixture")]
    }

    fn read<'a>(
        &'a self,
        _progress: &'a dyn ImportProgress,
    ) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
        let records = self.records.clone();
        Box::pin(async move { Ok(records) })
    }
}

#[tokio::test]
async fn decorator_expands_inner_records_and_delegates_capabilities() {
    let inner = FixtureConnector {
        records: vec![
            record("mem://plain.md", b"# hi".to_vec()),
            record("mem://bundle.tar", tar_bytes(&[("x.txt", b"x")])),
        ],
    };
    let connector = EnvelopeConnector::with_default_limits(Box::new(inner));
    assert_eq!(
        connector.capabilities(Effect::Read),
        vec![Capability::new(Effect::Read, Transport::InMemory, "fixture")]
    );
    let records = connector.read(&NoProgress).await.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].origin, "mem://plain.md");
    assert_eq!(records[1].origin, "mem://bundle.tar!x.txt");
}

#[tokio::test]
async fn decorator_delegates_write_to_inner() {
    let connector = EnvelopeConnector::with_default_limits(Box::new(FixtureConnector {
        records: Vec::new(),
    }));
    let error = connector
        .write(WriteRequest {
            origin: "mem://x".to_string(),
            content_type: "text/plain".to_string(),
            bytes: Vec::new(),
            metadata: BTreeMap::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ImportError::UnsupportedEffect {
            effect: Effect::Write
        }
    );
}
