//! Fixture-backend tests: no sockets, no sshd. [`Ssh2Backend`] is exercised
//! only by construction; its wire behavior needs a real server and stays out
//! of the unit suite.

use super::*;
use data_loader::NoProgress;

struct FixtureBackend {
    result: Result<Vec<u8>, String>,
}

impl SshBackend for FixtureBackend {
    fn fetch(&self, config: &SshConfig) -> Result<Vec<u8>, ImportError> {
        self.result.clone().map_err(|message| ImportError::SourceRead {
            origin: config.scope(),
            message,
        })
    }
}

fn connector(config: SshConfig, result: Result<Vec<u8>, String>) -> SshConnector {
    SshConnector::new(config, Box::new(FixtureBackend { result })).unwrap()
}

#[test]
fn capability_scope_is_ssh_user_host_path() {
    let connector = connector(
        SshConfig::new("files.example.com", "alice", "/etc/motd"),
        Ok(Vec::new()),
    );
    assert_eq!(
        connector.capabilities(Effect::Read),
        vec![Capability::new(
            Effect::Read,
            Transport::Ssh,
            "ssh://alice@files.example.com/etc/motd"
        )]
    );
    assert_eq!(
        connector.capabilities(Effect::Watch),
        vec![Capability::new(
            Effect::Watch,
            Transport::Ssh,
            "ssh://alice@files.example.com/etc/motd"
        )]
    );
    assert!(connector.capabilities(Effect::Write).is_empty());
}

#[tokio::test]
async fn emits_one_record_guessing_content_type_from_path() {
    let connector = connector(
        SshConfig::new("files.example.com", "alice", "/srv/data/things.json"),
        Ok(b"{}".to_vec()),
    );
    let records = connector.read(&NoProgress).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].origin, "ssh://alice@files.example.com/srv/data/things.json");
    assert_eq!(records[0].content_type, "application/json");
    assert_eq!(records[0].bytes, b"{}");
}

#[tokio::test]
async fn enforces_byte_cap_on_backend_output() {
    let connector = connector(
        SshConfig::new("h", "u", "/f.bin").with_max_response_bytes(4),
        Ok(vec![0u8; 5]),
    );
    let error = connector.read(&NoProgress).await.unwrap_err();
    assert!(matches!(error, ImportError::SourceRead { .. }));
    assert!(error.to_string().contains("byte bound"));
}

#[tokio::test]
async fn backend_error_propagates() {
    let connector = connector(
        SshConfig::new("h", "u", "/f.bin"),
        Err("channel read failed".to_string()),
    );
    let error = connector.read(&NoProgress).await.unwrap_err();
    assert_eq!(
        error,
        ImportError::SourceRead {
            origin: "ssh://u@h/f.bin".to_string(),
            message: "channel read failed".to_string(),
        }
    );
}

#[test]
fn rejects_invalid_config() {
    for config in [
        SshConfig::new("", "u", "/f"),
        SshConfig::new("h", "", "/f"),
        SshConfig::new("h", "u", ""),
        SshConfig::new("h", "u", "/f").with_max_response_bytes(0),
    ] {
        assert!(matches!(
            config.validate(),
            Err(ImportError::InvalidDescriptor { .. })
        ));
    }
}

#[test]
fn debug_redacts_passphrase() {
    let config = SshConfig::new("h", "u", "/f").with_auth(SshAuth::PrivateKey {
        path: PathBuf::from("/run/keys/id_ed25519"),
        passphrase: Some("hunter2".to_string()),
    });
    let debug = format!("{config:?}");
    assert!(!debug.contains("hunter2"), "passphrase leaked: {debug}");
    assert!(debug.contains("<redacted>"));
    let connector = connector(config, Ok(Vec::new()));
    let debug = format!("{connector:?}");
    assert!(!debug.contains("hunter2"), "passphrase leaked: {debug}");
}

#[test]
fn shell_quote_escapes_single_quotes() {
    assert_eq!(shell_quote("/etc/motd"), "'/etc/motd'");
    assert_eq!(shell_quote("a'b"), "'a'\\''b'");
}
