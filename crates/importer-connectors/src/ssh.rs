//! SSH byte connector — read one remote file over an exec channel.
//!
//! [`SshBackend`] is the only effectful seam: one fetch returning bytes.
//! [`Ssh2Backend`] is the production implementation (libssh2 via the `ssh2`
//! crate, vendored OpenSSL so the nix build needs no system libraries); tests
//! substitute fixtures and never open a socket. Credentials are runtime
//! configuration: the key path and passphrase never come from a package, the
//! passphrase is redacted from `Debug`, and no error message includes key
//! material.

use std::fmt;
use std::io::Read;
use std::path::PathBuf;

use data_loader::{
    Capability, Effect, ImportError, ImportFuture, SourceConnector, SourceRecord, Transport,
};

use crate::guess_content_type;

/// Default response bound: 64 MiB.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Hard bound on the configured response cap.
const HARD_MAX_RESPONSE_BYTES: usize = 1024 * 1024 * 1024;

/// Maximum bytes of remote stderr included in an error message.
const STDERR_EXCERPT_BYTES: usize = 256;

/// How the SSH session authenticates. Agent authentication never touches key
/// material in-process; explicit keys are read from a runtime-configured
/// path and are never logged.
#[derive(Clone)]
pub enum SshAuth {
    /// Authenticate through the user's running ssh-agent.
    Agent,
    /// Authenticate with an explicit private key file.
    PrivateKey {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

impl fmt::Debug for SshAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SshAuth::Agent => f.write_str("Agent"),
            SshAuth::PrivateKey { path, .. } => f
                .debug_struct("PrivateKey")
                .field("path", path)
                .field("passphrase", &"<redacted>")
                .finish(),
        }
    }
}

/// Runtime configuration for one SSH source.
#[derive(Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
    pub remote_path: String,
    pub max_response_bytes: usize,
    /// When set, the server host key must match this OpenSSH known_hosts
    /// file before any credential is sent. When unset, the first-seen host
    /// key is trusted — acceptable only inside trusted networks.
    pub known_hosts: Option<PathBuf>,
}

impl fmt::Debug for SshConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("auth", &self.auth)
            .field("remote_path", &self.remote_path)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("known_hosts", &self.known_hosts)
            .finish()
    }
}

impl SshConfig {
    pub fn new(
        host: impl Into<String>,
        username: impl Into<String>,
        remote_path: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port: 22,
            username: username.into(),
            auth: SshAuth::Agent,
            remote_path: remote_path.into(),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            known_hosts: None,
        }
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_auth(mut self, auth: SshAuth) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    pub fn with_known_hosts(mut self, known_hosts: impl Into<PathBuf>) -> Self {
        self.known_hosts = Some(known_hosts.into());
        self
    }

    /// Capability scope: `ssh://user@host/path`.
    pub fn scope(&self) -> String {
        format!(
            "ssh://{}@{}/{}",
            self.username,
            self.host,
            self.remote_path.trim_start_matches('/')
        )
    }

    pub fn validate(&self) -> Result<(), ImportError> {
        if self.host.trim().is_empty() || self.username.trim().is_empty() {
            return Err(ImportError::InvalidDescriptor {
                message: "ssh: host and username must be non-empty".to_string(),
            });
        }
        if self.remote_path.trim().is_empty() {
            return Err(ImportError::InvalidDescriptor {
                message: "ssh: remote_path must be non-empty".to_string(),
            });
        }
        if self.max_response_bytes == 0 || self.max_response_bytes > HARD_MAX_RESPONSE_BYTES {
            return Err(ImportError::InvalidDescriptor {
                message: format!(
                    "ssh: max_response_bytes must be between 1 and {HARD_MAX_RESPONSE_BYTES}, got {}",
                    self.max_response_bytes
                ),
            });
        }
        Ok(())
    }
}

/// Effectful SSH boundary. One method: read a remote file's bytes. The
/// backend owns transport, authentication, and remote command execution; the
/// connector owns capability declaration, record shape, and the byte cap.
pub trait SshBackend: Send + Sync {
    fn fetch(&self, config: &SshConfig) -> Result<Vec<u8>, ImportError>;
}

/// Effectful source connector for one remote file over SSH. Emits exactly
/// one [`SourceRecord`] per read; content type is guessed from the remote
/// file name since the transport carries none.
pub struct SshConnector {
    config: SshConfig,
    backend: Box<dyn SshBackend>,
}

impl fmt::Debug for SshConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshConnector")
            .field("config", &self.config)
            .field("backend", &"<SshBackend>")
            .finish()
    }
}

impl SshConnector {
    pub fn new(config: SshConfig, backend: Box<dyn SshBackend>) -> Result<Self, ImportError> {
        config.validate()?;
        Ok(Self { config, backend })
    }

    /// Production constructor over libssh2.
    pub fn production(config: SshConfig) -> Result<Self, ImportError> {
        Self::new(config, Box::new(Ssh2Backend))
    }

    pub fn config(&self) -> &SshConfig {
        &self.config
    }
}

impl SourceConnector for SshConnector {
    fn capabilities(&self, effect: Effect) -> Vec<Capability> {
        match effect {
            Effect::Read | Effect::Watch => {
                vec![Capability::new(effect, Transport::Ssh, self.config.scope())]
            }
            _ => Vec::new(),
        }
    }

    fn read<'a>(&'a self) -> ImportFuture<'a, Result<Vec<SourceRecord>, ImportError>> {
        Box::pin(async move {
            let bytes = self.backend.fetch(&self.config)?;
            if bytes.len() > self.config.max_response_bytes {
                return Err(ImportError::SourceRead {
                    origin: self.config.scope(),
                    message: format!(
                        "remote file exceeds the {} byte bound",
                        self.config.max_response_bytes
                    ),
                });
            }
            Ok(vec![SourceRecord {
                origin: self.config.scope(),
                content_type: guess_content_type(&self.config.remote_path).to_string(),
                bytes,
                metadata: Default::default(),
            }])
        })
    }
}

/// Quote a path for POSIX shell: single-quote wrapped, embedded quotes
/// escaped as `'\''`.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// Production [`SshBackend`] over libssh2. Reads the remote file by exec'ing
/// `cat` on a session channel and streaming stdout with an early byte cap.
pub struct Ssh2Backend;

impl SshBackend for Ssh2Backend {
    fn fetch(&self, config: &SshConfig) -> Result<Vec<u8>, ImportError> {
        let origin = config.scope();
        let fail = |message: String| ImportError::SourceRead {
            origin: origin.clone(),
            message,
        };

        let tcp = std::net::TcpStream::connect((config.host.as_str(), config.port))
            .map_err(|error| fail(format!("TCP connect failed: {error}")))?;
        let mut session =
            ssh2::Session::new().map_err(|error| fail(format!("session init failed: {error}")))?;
        session.set_tcp_stream(tcp);
        session
            .handshake()
            .map_err(|error| fail(format!("handshake failed: {error}")))?;

        if let Some(known_hosts) = &config.known_hosts {
            let mut hosts = session
                .known_hosts()
                .map_err(|error| fail(format!("known_hosts init failed: {error}")))?;
            hosts
                .read_file(known_hosts, ssh2::KnownHostFileKind::OpenSSH)
                .map_err(|error| {
                    fail(format!(
                        "known_hosts file {} unreadable: {error}",
                        known_hosts.display()
                    ))
                })?;
            let (key, _kind) = session
                .host_key()
                .ok_or_else(|| fail("server presented no host key".to_string()))?;
            match hosts.check_port(&config.host, config.port, key) {
                ssh2::CheckResult::Match => {}
                result => {
                    return Err(fail(format!(
                        "host key verification failed: {result:?}"
                    )))
                }
            }
        }

        match &config.auth {
            SshAuth::Agent => session
                .userauth_agent(&config.username)
                .map_err(|error| fail(format!("agent authentication failed: {error}")))?,
            SshAuth::PrivateKey { path, passphrase } => session
                .userauth_pubkey_file(
                    &config.username,
                    None,
                    path,
                    passphrase.as_deref(),
                )
                .map_err(|error| {
                    fail(format!(
                        "public-key authentication with {} failed: {error}",
                        path.display()
                    ))
                })?,
        }
        if !session.authenticated() {
            return Err(fail("authentication did not complete".to_string()));
        }

        let mut channel = session
            .channel_session()
            .map_err(|error| fail(format!("channel open failed: {error}")))?;
        channel
            .exec(&format!("cat {}", shell_quote(&config.remote_path)))
            .map_err(|error| fail(format!("remote exec failed: {error}")))?;

        let mut bytes = Vec::new();
        let mut chunk = [0u8; 32 * 1024];
        loop {
            let read = channel
                .read(&mut chunk)
                .map_err(|error| fail(format!("channel read failed: {error}")))?;
            if read == 0 {
                break;
            }
            if bytes.len() + read > config.max_response_bytes {
                return Err(fail(format!(
                    "remote file exceeds the {} byte bound",
                    config.max_response_bytes
                )));
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
        let mut stderr = Vec::new();
        let _ = channel.stderr().read_to_end(&mut stderr);
        channel
            .wait_close()
            .map_err(|error| fail(format!("channel close failed: {error}")))?;
        let status = channel
            .exit_status()
            .map_err(|error| fail(format!("exit status unavailable: {error}")))?;
        if status != 0 {
            let excerpt = String::from_utf8_lossy(&stderr[..stderr.len().min(STDERR_EXCERPT_BYTES)])
                .into_owned();
            return Err(fail(format!(
                "remote command exited {status}: {excerpt}"
            )));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests;
