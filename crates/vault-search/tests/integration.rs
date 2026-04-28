use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn fixture_vault(include_hippo: bool) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::write(
        root.join("notes/alpha.md"),
        "---\ntitle: Alpha\ntags: [foo, bar]\n---\n# Alpha\n\nHello indexable world. The quick brown fox.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("notes/beta.md"),
        "# Beta runbook\n\nA short runbook discussing kafka and other systems.\n",
    )
    .unwrap();
    if include_hippo {
        std::fs::create_dir_all(root.join("30-Knowledge-Base/_hippo")).unwrap();
        std::fs::write(
            root.join("30-Knowledge-Base/_hippo/secret.md"),
            "# Hippo memory\n\nContains hippo private text only matchable when included.\n",
        )
        .unwrap();
    }
    tmp
}

fn binary_path() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set for integration tests by Cargo.
    PathBuf::from(env!("CARGO_BIN_EXE_vault-search"))
}

struct ServerHandle {
    child: Child,
    port: u16,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(vault: &std::path::Path, extra_args: &[&str]) -> ServerHandle {
    let cache = tempfile::tempdir().unwrap().keep();
    let mut cmd = Command::new(binary_path());
    cmd.arg("--vault")
        .arg(vault)
        .arg("--port")
        .arg("0")
        .arg("--cache-dir")
        .arg(&cache)
        .arg("--log")
        .arg("warn");
    for a in extra_args {
        cmd.arg(a);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn vault-search");

    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr);

    let mut port: Option<u16> = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap_or(0);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if let Some(rest) = line.find("listening on http://") {
            let after = &line[rest + "listening on http://".len()..];
            if let Some((_, p)) = after.trim().rsplit_once(':') {
                if let Ok(parsed) = p.parse::<u16>() {
                    port = Some(parsed);
                    break;
                }
            }
        }
    }
    let port = port.expect("did not see listening line on stderr");
    ServerHandle { child, port }
}

fn get_blocking(url: &str) -> (u16, String) {
    let resp = ureq_get(url);
    resp
}

// Tiny blocking HTTP GET to avoid pulling in a heavy client; integration
// tests just need status + body. Uses std::net + raw HTTP/1.1.
fn ureq_get(url: &str) -> (u16, String) {
    let url = url.strip_prefix("http://").expect("http://");
    let (host_port, path) = match url.find('/') {
        Some(i) => (&url[..i], &url[i..]),
        None => (url, "/"),
    };
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let mut stream = TcpStream::connect(host_port).expect("connect");
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).unwrap();
    let mut parts = buf.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap();
    let body = parts.next().unwrap_or("");
    let status_line = head.lines().next().unwrap();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // Some axum responses come back chunked. Strip trailing chunk markers
    // crudely if present (the JSON we want sits at the start of `body` for
    // small responses, but a chunked response prefixes with a hex length).
    let body = strip_chunked(body);
    (code, body)
}

fn strip_chunked(body: &str) -> String {
    // If first line parses as hex, treat as chunked. Otherwise return as-is.
    let first_line = body.lines().next().unwrap_or("");
    if u64::from_str_radix(first_line.trim(), 16).is_ok() {
        let mut out = String::new();
        let mut rest = body;
        loop {
            let nl = match rest.find("\r\n") {
                Some(i) => i,
                None => break,
            };
            let len_hex = rest[..nl].trim();
            let n = match u64::from_str_radix(len_hex, 16) {
                Ok(v) => v as usize,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let chunk_start = nl + 2;
            let chunk_end = chunk_start + n;
            if chunk_end > rest.len() {
                break;
            }
            out.push_str(&rest[chunk_start..chunk_end]);
            // skip trailing \r\n
            rest = &rest[chunk_end + 2..];
        }
        return out;
    }
    body.to_string()
}

#[test]
fn full_e2e_default_excludes_hippo() {
    let vault = fixture_vault(true);
    let server = spawn_server(vault.path(), &[]);

    let base = format!("http://127.0.0.1:{}", server.port);
    let (code, body) = get_blocking(&format!("{base}/health"));
    assert_eq!(code, 200, "health body: {body}");
    assert!(body.contains("\"status\":\"ready\""), "body: {body}");
    // 2 fixture notes — no hippo by default.
    assert!(body.contains("\"indexed\":2"), "body: {body}");

    // Search for content unique to alpha.
    let (code, body) = get_blocking(&format!("{base}/search?q=indexable"));
    assert_eq!(code, 200);
    assert!(body.contains("notes/alpha"), "body: {body}");
    assert!(body.contains("snippet"), "body: {body}");

    // /ids hot path.
    let (code, body) = get_blocking(&format!("{base}/ids?q=runbook"));
    assert_eq!(code, 200);
    assert!(body.contains("notes/beta"), "body: {body}");

    // /node lookup with URL-encoded id.
    let (code, body) = get_blocking(&format!("{base}/node/notes%2Falpha"));
    assert_eq!(code, 200);
    assert!(body.contains("\"title\":\"Alpha\""), "body: {body}");
    assert!(body.contains("foo"), "body: {body}");

    // Hippo should NOT match.
    let (code, body) = get_blocking(&format!("{base}/ids?q=hippo"));
    assert_eq!(code, 200);
    assert!(body.contains("\"total\":0"), "expected zero hippo: {body}");
}

#[test]
fn include_hippo_indexes_hippo_files() {
    let vault = fixture_vault(true);
    let server = spawn_server(vault.path(), &["--include-hippo"]);

    let base = format!("http://127.0.0.1:{}", server.port);
    let (code, body) = get_blocking(&format!("{base}/health"));
    assert_eq!(code, 200);
    assert!(body.contains("\"indexed\":3"), "body: {body}");

    let (code, body) = get_blocking(&format!("{base}/ids?q=hippo"));
    assert_eq!(code, 200);
    assert!(body.contains("_hippo/secret"), "body: {body}");
}
