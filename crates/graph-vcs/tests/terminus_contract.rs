//! TerminusDB backend tests for [`TerminusStore`]: the shared [`contract`]
//! parity suite (the same scenarios [`MinigrafStore`] passes) plus
//! TerminusDB-specific persistence coverage. Two gates, both auto-skipping:
//!
//! - **`TERMINUSDB_TEST_URL`** — run each contract scenario against an
//!   already-running server:
//!   ```sh
//!   TERMINUSDB_TEST_URL=http://127.0.0.1:6363 \
//!     cargo test -p graph-vcs --features terminus,contract --test terminus_contract
//!   ```
//!   `TERMINUSDB_TEST_ORG` / `TERMINUSDB_TEST_USER` / `TERMINUSDB_TEST_PASSWORD`
//!   (defaults `jump_cannon` / `admin` / `root`) complete the config.
//! - **`TERMINUSDB_BOOT_CONTAINER=docker|podman`** (or `1` to autodetect) —
//!   one test boots `terminusdb/terminusdb-server` itself
//!   (`TERMINUSDB_TEST_IMAGE` overrides the pinned image), waits for
//!   `/api/ok`, runs the entire contract plus the persistence test, and
//!   tears the container down.
//!
//! With neither set every test skips, so a plain
//! `cargo test -p graph-vcs --all-features` passes without docker. Each
//! scenario uses its own database (= world), so tests are independent,
//! repeatable, and parallel-safe.

#![cfg(all(feature = "terminus", feature = "contract", not(target_arch = "wasm32")))]

use graph_vcs::contract;
use graph_vcs::model::NodeId;
use graph_vcs::{TerminusConfig, TerminusStore, VcsStore};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const IMAGE: &str = "docker.io/terminusdb/terminusdb-server:v12.0.7";

/// A store backed by a fresh, uniquely named database (= world).
fn store(config: &TerminusConfig, test: &str) -> TerminusStore {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let world = format!("t{}-{test}-{seq}", std::process::id());
    // World names are embedded in URL path segments.
    let world = world.replace('_', "-");
    TerminusStore::new(config.clone(), &world).expect("connect + create database")
}

fn env_config() -> Option<TerminusConfig> {
    let base_url = std::env::var("TERMINUSDB_TEST_URL").ok()?;
    Some(TerminusConfig {
        base_url,
        org: std::env::var("TERMINUSDB_TEST_ORG").unwrap_or_else(|_| "jump_cannon".into()),
        user: std::env::var("TERMINUSDB_TEST_USER").unwrap_or_else(|_| "admin".into()),
        password: std::env::var("TERMINUSDB_TEST_PASSWORD").unwrap_or_else(|_| "root".into()),
    })
}

/// The env-gated config, or skip the calling test.
macro_rules! config_or_skip {
    () => {
        match env_config() {
            Some(config) => config,
            None => {
                eprintln!("skip: TERMINUSDB_TEST_URL is not set");
                return;
            }
        }
    };
}

#[test]
fn commit_materialize_roundtrip() {
    let config = config_or_skip!();
    contract::commit_materialize_roundtrip(&store(&config, "roundtrip"));
}

#[test]
fn large_node_roundtrip() {
    let config = config_or_skip!();
    contract::large_node_roundtrip(&store(&config, "large"));
}

#[test]
fn branch_and_diff() {
    let config = config_or_skip!();
    contract::branch_and_diff(&store(&config, "branch"));
}

#[test]
fn merge_disjoint_nodes_auto_merges() {
    let config = config_or_skip!();
    contract::merge_disjoint_nodes_auto_merges(&store(&config, "mdisjoint"));
}

#[test]
fn merge_different_frontmatter_keys_auto_merges() {
    let config = config_or_skip!();
    contract::merge_different_frontmatter_keys_auto_merges(&store(&config, "mfmkeys"));
}

#[test]
fn merge_same_attribute_conflicts_then_resolve() {
    let config = config_or_skip!();
    contract::merge_same_attribute_conflicts_then_resolve(&store(&config, "mconflict"));
}

#[test]
fn merge_delete_vs_edit_conflicts() {
    let config = config_or_skip!();
    contract::merge_delete_vs_edit_conflicts(&store(&config, "mdelete"));
}

#[test]
fn edge_merge_semantics() {
    let config = config_or_skip!();
    contract::edge_merge_semantics(&store(&config, "edges"));
}

#[test]
fn rebase_replays_commits() {
    let config = config_or_skip!();
    contract::rebase_replays_commits(&store(&config, "rebase"));
}

#[test]
fn op_log_records_operations_in_order() {
    let config = config_or_skip!();
    contract::op_log_records_operations_in_order(&store(&config, "oplog"));
}

/// TerminusDB-specific: state is server-side, so a second store instance
/// over the same database (= world) sees the committed head, log, branches,
/// and op log.
#[test]
fn reopen_preserves_state() {
    let config = config_or_skip!();
    let first = store(&config, "reopen");
    let commit = contract::block_on(first.commit(
        "main",
        vec![graph_vcs::model::GraphOp::UpsertNode(vault_data::VaultNode {
            id: "persistent".into(),
            ..Default::default()
        })],
        "alice",
        "durable",
    ))
    .unwrap();
    contract::block_on(first.create_branch("dev", &commit.id)).unwrap();

    let second = TerminusStore::new(config, &first.world().to_string()).unwrap();
    assert_eq!(
        contract::block_on(second.head("main")).unwrap(),
        Some(commit.id.clone())
    );
    let log = contract::block_on(second.log("main", 10)).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].id, commit.id);
    assert_eq!(log[0].change_id, commit.change_id);
    let snapshot = contract::block_on(second.materialize(&commit.id)).unwrap();
    assert!(snapshot.nodes.contains_key(&NodeId("persistent".into())));
    assert_eq!(contract::block_on(second.branches()).unwrap().len(), 2);
    assert_eq!(contract::block_on(second.op_log(10)).unwrap().len(), 2);
}

// ── Container-booting harness ───────────────────────────────────────────────

/// A booted server container; dropped (force-removed) at scope exit.
struct BootedServer {
    cli: String,
    container: String,
    config: TerminusConfig,
}

impl Drop for BootedServer {
    fn drop(&mut self) {
        let _ = Command::new(&self.cli)
            .args(["rm", "-f", &self.container])
            .output();
    }
}

/// Pick a free loopback port (released before the container binds it — the
/// small race is acceptable for a test harness).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn cli_available(cli: &str) -> bool {
    Command::new(cli)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Boot `terminusdb/terminusdb-server` and wait for `/api/ok`. Returns None
/// (skip) when booting is not requested or no requested CLI exists.
fn boot_server() -> Option<BootedServer> {
    let cli = match std::env::var("TERMINUSDB_BOOT_CONTAINER").ok()?.as_str() {
        "" | "0" | "false" => return None,
        cli @ ("docker" | "podman") => {
            if !cli_available(cli) {
                eprintln!("skip: requested container CLI {cli:?} is not available");
                return None;
            }
            cli.to_string()
        }
        // "1"/"true"/anything else: autodetect.
        _ => ["docker", "podman"]
            .into_iter()
            .find(|cli| cli_available(cli))
            .map(str::to_string)
            .or_else(|| {
                eprintln!("skip: neither docker nor podman is available");
                None
            })?,
    };
    let image = std::env::var("TERMINUSDB_TEST_IMAGE").unwrap_or_else(|_| IMAGE.into());
    let port = free_port();
    let container = format!("jc-terminus-contract-{}", std::process::id());
    let output = Command::new(&cli)
        .args([
            "run",
            "--rm",
            "-d",
            "--name",
            &container,
            "-p",
            &format!("127.0.0.1:{port}:6363"),
            "-e",
            "TERMINUSDB_ADMIN_PASS=root",
            &image,
        ])
        .output()
        .expect("spawn container runtime");
    if !output.status.success() {
        panic!(
            "container boot failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let base_url = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if client
            .get(format!("{base_url}/api/ok"))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            break;
        }
        assert!(Instant::now() < deadline, "terminusdb did not become ready");
        std::thread::sleep(Duration::from_millis(500));
    }
    Some(BootedServer {
        cli,
        container,
        config: TerminusConfig {
            base_url,
            org: "jump_cannon".into(),
            user: "admin".into(),
            password: "root".into(),
        },
    })
}

/// Boot a fresh server and run the whole contract plus the persistence
/// scenario against it in one test (sequential, so the container lifecycle
/// is a single scope).
#[test]
fn booted_container_full_contract() {
    let Some(server) = boot_server() else {
        eprintln!("skip: TERMINUSDB_BOOT_CONTAINER is not set");
        return;
    };
    contract::run_all(|name| store(&server.config, name));
    // Persistence across store instances over the same database.
    let first = store(&server.config, "bootreopen");
    let commit = contract::block_on(first.commit(
        "main",
        vec![graph_vcs::model::GraphOp::UpsertNode(vault_data::VaultNode {
            id: "persistent".into(),
            ..Default::default()
        })],
        "alice",
        "durable",
    ))
    .unwrap();
    let world = first.world().to_string();
    drop(first);
    let second = TerminusStore::new(server.config.clone(), &world).unwrap();
    assert_eq!(
        contract::block_on(second.head("main")).unwrap(),
        Some(commit.id)
    );
}
