//! Rust-driven browser regression suite (foundation).
//!
//! Asserts the bare minimum that future regression checks will build on:
//!
//!   1. The page at `--base-url` responds (HTTP 200).
//!   2. Headless Chromium launches with WebGPU flags and navigates.
//!   3. The boot log line `[jump-cannon-ui] boot` appears on the JS
//!      console within `--timeout-secs`.
//!   4. The graph `<canvas>` element exists with non-zero width/height.
//!   5. A screenshot is saved to `<out-dir>/boot.png` for visual review.
//!
//! Anything flaky (pixel brightness, motion deltas, click recovery) is
//! deliberately deferred. (The legacy egui-era Playwright suite that held
//! those checks was removed with the egui frontend — see git history.)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams;
use clap::Parser;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// The single readiness signal we wait for. Logged from
/// `app/ui/src/main.rs` (fn main) right before the Dioxus app launches.
const BOOT_LOG_NEEDLE: &str = "[jump-cannon-ui] boot";

#[derive(Parser, Debug)]
#[command(name = "test-browser", about = "Rust-driven browser smoke test")]
struct Args {
    /// Base URL of a running graph-api server (e.g. http://localhost:8765).
    #[arg(long)]
    base_url: String,

    /// Path to a Chromium / Chrome executable.
    #[arg(long)]
    chromium: PathBuf,

    /// Directory to write `boot.png` and `report.json` into.
    #[arg(long, default_value = "target/test-browser-rust")]
    out_dir: PathBuf,

    /// Overall test timeout (seconds).
    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,
}

#[derive(Serialize)]
struct Report {
    ok: bool,
    base_url: String,
    canvas_width: u32,
    canvas_height: u32,
    boot_log_found: bool,
    duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_header_actions: Option<HeaderActionCheck>,
    page_errors: Vec<String>,
    console_logs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct HeaderActionDetail {
    label: String,
    title: String,
    width: f64,
    height: f64,
    within_header: bool,
    within_panel: bool,
    drag_started: bool,
    canvas_after_click: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct HeaderActionCheck {
    ok: bool,
    boot_handoff_ok: bool,
    workspace_mode: String,
    action_count: usize,
    play_pause_found: bool,
    fit_found: bool,
    geometry_ok: bool,
    clicks_ok: bool,
    drag_started: bool,
    canvas_present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    actions: Vec<HeaderActionDetail>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    tokio::fs::create_dir_all(&args.out_dir)
        .await
        .with_context(|| format!("create out_dir {}", args.out_dir.display()))?;

    let started = Instant::now();
    let console_logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let result = run(&args, console_logs.clone()).await;
    let duration_ms = started.elapsed().as_millis();

    let logs = console_logs.lock().await.clone();
    let (ok, reason, canvas_width, canvas_height, boot_log_found, header_actions, page_errors) =
        match &result {
            Ok(o) => {
                let ok = o.header_actions.ok && o.page_errors.is_empty();
                let reason = if !o.header_actions.ok {
                    o.header_actions.reason.clone()
                } else if !o.page_errors.is_empty() {
                    Some(format!(
                        "browser emitted {} console error(s) or unhandled exception(s)",
                        o.page_errors.len()
                    ))
                } else {
                    None
                };
                (
                    ok,
                    reason,
                    o.canvas_width,
                    o.canvas_height,
                    o.boot_log_found,
                    Some(o.header_actions.clone()),
                    o.page_errors.clone(),
                )
            }
            Err(e) => (false, Some(format!("{e:#}")), 0, 0, false, None, Vec::new()),
        };

    let report = Report {
        ok,
        base_url: args.base_url.clone(),
        canvas_width,
        canvas_height,
        boot_log_found,
        duration_ms,
        reason: reason.clone(),
        graph_header_actions: header_actions,
        page_errors,
        console_logs: tail(&logs, 50),
    };

    let report_path = args.out_dir.join("report.json");
    tokio::fs::write(&report_path, serde_json::to_vec_pretty(&report)?).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);

    if !ok {
        tracing::error!(
            "test-browser failed: {}",
            reason.unwrap_or_else(|| "unknown".to_string())
        );
        std::process::exit(1);
    }
    Ok(())
}

struct RunOk {
    canvas_width: u32,
    canvas_height: u32,
    boot_log_found: bool,
    header_actions: HeaderActionCheck,
    page_errors: Vec<String>,
}

async fn run(args: &Args, console_logs: Arc<Mutex<Vec<String>>>) -> Result<RunOk> {
    // ---- 1. server reachability ------------------------------------------
    let probe_url = args.base_url.trim_end_matches('/').to_string() + "/";
    probe_server(&probe_url, Duration::from_secs(args.timeout_secs.min(30))).await?;

    // ---- 2. launch chromium ----------------------------------------------
    let mut chromium_args: Vec<String> = vec![
        "--enable-unsafe-webgpu".into(),
        "--no-sandbox".into(),
        "--disable-dev-shm-usage".into(),
        "--disable-gpu-sandbox".into(),
        "--window-size=1280,800".into(),
    ];
    // Headless Linux uses the Nix-provided lavapipe Vulkan adapter. Installed
    // Chrome on macOS should retain its native Metal/ANGLE defaults.
    if cfg!(target_os = "linux") {
        chromium_args.extend([
            "--enable-features=Vulkan".into(),
            "--use-angle=vulkan".into(),
            "--use-gl=angle".into(),
        ]);
    }

    let config = BrowserConfig::builder()
        .chrome_executable(&args.chromium)
        .args(chromium_args)
        .window_size(1280, 800)
        .build()
        .map_err(|e| anyhow!("BrowserConfig: {e}"))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .context("Browser::launch")?;

    // The CDP handler must be driven; spawn a task that polls it.
    let handler_task = tokio::spawn(async move {
        while let Some(_) = handler.next().await {}
    });

    let outcome = drive_page(&browser, args, console_logs).await;

    // Best-effort browser teardown.
    let _ = browser.close().await;
    drop(browser);
    let _ = handler_task.await;

    outcome
}

async fn drive_page(
    browser: &Browser,
    args: &Args,
    console_logs: Arc<Mutex<Vec<String>>>,
) -> Result<RunOk> {
    use chromiumoxide::cdp::browser_protocol::page::EventLifecycleEvent;

    let page = browser
        .new_page("about:blank")
        .await
        .context("new_page")?;

    // Console listener. Push every entry; we filter for the boot needle
    // when polling below.
    let mut console_events = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::log::EventEntryAdded>()
        .await
        .context("listen log entries")?;
    let mut runtime_console = page
        .event_listener::<chromiumoxide::cdp::js_protocol::runtime::EventConsoleApiCalled>()
        .await
        .context("listen console api")?;
    let mut runtime_exceptions = page
        .event_listener::<chromiumoxide::cdp::js_protocol::runtime::EventExceptionThrown>()
        .await
        .context("listen runtime exceptions")?;
    // Suppress unused warning on lifecycle stream (kept in case future
    // checks want to wait for `load` semantically).
    let _ = page
        .event_listener::<EventLifecycleEvent>()
        .await
        .ok();

    let logs_a = console_logs.clone();
    let console_pump = tokio::spawn(async move {
        while let Some(ev) = console_events.next().await {
            let line = format!("[log] {}", ev.entry.text);
            logs_a.lock().await.push(line);
        }
    });
    let logs_b = console_logs.clone();
    let runtime_pump = tokio::spawn(async move {
        while let Some(ev) = runtime_console.next().await {
            // EventConsoleApiCalled carries an Args array of RemoteObjects;
            // we stringify each Arg's `value` / `description` for diagnostics.
            let parts: Vec<String> = ev
                .args
                .iter()
                .map(|a| {
                    a.value
                        .as_ref()
                        .map(|v| v.to_string())
                        .or_else(|| a.description.clone())
                        .unwrap_or_default()
                })
                .collect();
            let line = format!("[{}] {}", ev.r#type.as_ref(), parts.join(" "));
            logs_b.lock().await.push(line);
        }
    });
    let logs_c = console_logs.clone();
    let exception_pump = tokio::spawn(async move {
        while let Some(ev) = runtime_exceptions.next().await {
            let details = &ev.exception_details;
            let description = details
                .exception
                .as_ref()
                .and_then(|exception| exception.description.clone())
                .unwrap_or_default();
            let line = if description.is_empty() {
                format!("[exception] {}", details.text)
            } else {
                format!("[exception] {}: {}", details.text, description)
            };
            logs_c.lock().await.push(line);
        }
    });

    let target = args.base_url.trim_end_matches('/').to_string() + "/";
    tracing::info!("navigating to {target}");
    page.goto(&target).await.context("page.goto")?;
    page.wait_for_navigation().await.context("wait_for_navigation")?;

    // ---- 3. wait for the boot log line -----------------------------------
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let mut boot_log_found = false;
    while Instant::now() < deadline {
        {
            let logs = console_logs.lock().await;
            if logs.iter().any(|l| l.contains(BOOT_LOG_NEEDLE)) {
                boot_log_found = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if !boot_log_found {
        let recent = tail(&console_logs.lock().await, 20).join("\n");
        // Don't bail yet — still capture screenshot + canvas info for
        // diagnostics, but mark the failure so we exit non-zero.
        tracing::warn!(
            "boot log {:?} not seen within {}s; recent console:\n{}",
            BOOT_LOG_NEEDLE,
            args.timeout_secs,
            recent
        );
    }

    // ---- 4. graph header actions are present, visible, and safe ----------
    // Wait for graph data to finish loading: the boot log is emitted before
    // the Graph panel's canvas and header actions necessarily exist.
    let graph_ready_js = r#"(() => {
        const panel = document.querySelector('section.panel-graph');
        const header = panel?.querySelector(':scope > header.panel-head');
        const canvas = panel?.querySelector('canvas.graph-canvas');
        const actions = header?.querySelectorAll(
          '.panel-head-actions button.panel-head-action'
        );
        return Boolean(panel && header && canvas && actions?.length >= 2);
    })()"#;
    let graph_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    while Instant::now() < graph_deadline {
        let ready: bool = page.evaluate(graph_ready_js).await?.into_value()?;
        if ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The reported clipping occurred in tiling mode. Fresh browser profiles
    // start floating, so switch through Panel Kit's real mode control before
    // measuring or taking the screenshot.
    let tiling_mode_js = r#"(async () => {
        const nextFrames = () => new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve))
        );
        let root = document.querySelector('.ws');
        if (!root?.classList.contains('tiling')) {
          const graph = document.querySelector('section.panel-graph');
          const modeButton = graph?.querySelector(':scope > header.panel-head .light.mode');
          modeButton?.click();
          await nextFrames();
          root = document.querySelector('.ws');
        }
        return root?.classList.contains('tiling') ?? false;
    })()"#;
    let tiling_mode: bool = page.evaluate(tiling_mode_js).await?.into_value()?;
    if !tiling_mode {
        bail!("failed to switch Panel Kit workspace into tiling mode");
    }

    // Exercise the controls through the same pointer/mouse event sequence a
    // user generates. Holding pointerdown across two animation frames catches
    // accidental propagation into Panel Kit's panel-drag handlers.
    let header_actions_js = r#"(async () => {
        const panel = document.querySelector('section.panel-graph');
        const header = panel?.querySelector(':scope > header.panel-head');
        const root = panel?.closest('.ws-root') || document.querySelector('.ws-root');
        const workspace = panel?.closest('.ws') || document.querySelector('.ws');
        const staticBoot = document.querySelector('[data-panel-kit-static-boot]');
        const bootHandoffOk = !staticBoot || getComputedStyle(staticBoot).display === 'none';
        const workspaceMode = workspace?.classList.contains('tiling') ? 'tiling' : 'floating';
        const buttons = header
          ? [...header.querySelectorAll('.panel-head-actions button.panel-head-action')]
          : [];
        const text = (button) => (button?.textContent || '').trim();
        const title = (button) => button?.getAttribute('title') || '';
        const isPlayPause = (button) =>
          text(button) === '▶' || text(button) === 'Ⅱ' ||
          /Pause|Resume|Freeze|Follow/i.test(title(button));
        const isFit = (button) => text(button) === 'Fit' || /Fit the camera/i.test(title(button));
        const playPause = buttons.find(isPlayPause);
        const fit = buttons.find(isFit);
        const chosen = [playPause, fit].filter(Boolean);
        const headerRect = header?.getBoundingClientRect();
        const panelRect = panel?.getBoundingClientRect();
        const details = chosen.map((button) => {
          const rect = button.getBoundingClientRect();
          return {
            label: text(button),
            title: title(button),
            width: rect.width,
            height: rect.height,
            within_header: Boolean(headerRect) &&
              rect.left >= headerRect.left - 0.5 && rect.right <= headerRect.right + 0.5,
            within_panel: Boolean(panelRect) &&
              rect.left >= panelRect.left - 0.5 && rect.right <= panelRect.right + 0.5,
            drag_started: false,
            canvas_after_click: false,
          };
        });
        const nextFrames = () => new Promise((resolve) =>
          requestAnimationFrame(() => requestAnimationFrame(resolve))
        );
        const dragActive = () => Boolean(
          root?.classList.contains('dragging') ||
          panel?.classList.contains('tile-dragging')
        );

        for (let i = 0; i < chosen.length; i += 1) {
          const button = chosen[i];
          const rect = button.getBoundingClientRect();
          const point = {
            bubbles: true,
            cancelable: true,
            button: 0,
            buttons: 1,
            clientX: rect.left + rect.width / 2,
            clientY: rect.top + rect.height / 2,
            pointerId: 1,
            pointerType: 'mouse',
            isPrimary: true,
          };
          let sawDrag = dragActive();
          const observer = new MutationObserver(() => { sawDrag ||= dragActive(); });
          if (root) {
            observer.observe(root, {
              attributes: true,
              subtree: true,
              attributeFilter: ['class'],
            });
          }
          button.dispatchEvent(new PointerEvent('pointerdown', point));
          button.dispatchEvent(new MouseEvent('mousedown', point));
          await nextFrames();
          sawDrag ||= dragActive();
          button.dispatchEvent(new PointerEvent('pointerup', { ...point, buttons: 0 }));
          button.dispatchEvent(new MouseEvent('mouseup', { ...point, buttons: 0 }));
          button.click();
          await nextFrames();
          sawDrag ||= dragActive();
          observer.disconnect();
          const canvas = document.querySelector('section.panel-graph canvas.graph-canvas');
          const canvasRect = canvas?.getBoundingClientRect();
          details[i].drag_started = sawDrag;
          details[i].canvas_after_click = Boolean(
            canvasRect && canvasRect.width > 0 && canvasRect.height > 0
          );
        }

        const playPauseFound = Boolean(playPause);
        const fitFound = Boolean(fit);
        const geometryOk = details.length === 2 && details.every((action) =>
          action.width > 0 && action.height > 0 &&
          action.within_header && action.within_panel
        );
        const dragStarted = details.some((action) => action.drag_started);
        const canvasPresent = details.length === 2 &&
          details.every((action) => action.canvas_after_click);
        const clicksOk = !dragStarted && canvasPresent;
        const failures = [];
        if (!bootHandoffOk) failures.push('static pre-WASM boot shell still visible');
        if (workspaceMode !== 'tiling') failures.push('workspace did not remain in tiling mode');
        if (!panel || !header) failures.push('Graph panel header missing');
        if (!playPauseFound) failures.push('play/pause action missing');
        if (!fitFound) failures.push('Fit action missing');
        if (!geometryOk) failures.push('header action clipped or zero-sized');
        if (dragStarted) failures.push('header action started a panel drag');
        if (!canvasPresent) failures.push('Graph canvas missing after action click');
        return {
          ok: failures.length === 0,
          boot_handoff_ok: bootHandoffOk,
          workspace_mode: workspaceMode,
          action_count: buttons.length,
          play_pause_found: playPauseFound,
          fit_found: fitFound,
          geometry_ok: geometryOk,
          clicks_ok: clicksOk,
          drag_started: dragStarted,
          canvas_present: canvasPresent,
          reason: failures.length ? failures.join('; ') : null,
          actions: details,
        };
    })()"#;
    let header_actions_value: serde_json::Value =
        page.evaluate(header_actions_js).await?.into_value()?;
    let header_actions: HeaderActionCheck = serde_json::from_value(header_actions_value)
        .context("decode Graph header action regression result")?;

    // ---- 5. canvas exists with non-zero size -----------------------------
    // The Dioxus app's graph canvas is `<canvas class="graph-canvas">`
    // (app/ui/src/graph_canvas.rs); fall back to any canvas on the page.
    let dims_js = r#"(() => {
        const c = document.querySelector('canvas.graph-canvas')
          || document.querySelector('canvas');
        if (!c) return { w: 0, h: 0 };
        const r = c.getBoundingClientRect();
        return {
          w: c.width || Math.round(r.width),
          h: c.height || Math.round(r.height),
        };
    })()"#;
    let dims: serde_json::Value = page.evaluate(dims_js).await?.into_value()?;
    let canvas_width = dims.get("w").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let canvas_height = dims.get("h").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    // ---- 6. screenshot ---------------------------------------------------
    let shot_params = CaptureScreenshotParams::builder().build();
    let png_b64 = page
        .screenshot(shot_params)
        .await
        .context("screenshot")?;
    let bytes: Vec<u8> = match png_b64 {
        // Newer chromiumoxide returns Vec<u8> directly; older returns base64
        // string. Handle both by trying to decode if it looks like base64.
        b => b,
    };
    let shot_path = args.out_dir.join("boot.png");
    // Heuristic: if first byte is the PNG magic 0x89, write as-is; else
    // assume base64-encoded text.
    if bytes.first() == Some(&0x89) {
        tokio::fs::write(&shot_path, &bytes).await?;
    } else {
        // base64-decode fallback (defensive — depends on chromiumoxide version).
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&bytes)
            .unwrap_or(bytes);
        tokio::fs::write(&shot_path, &decoded).await?;
    }
    tracing::info!("wrote screenshot {}", shot_path.display());

    // Tear down pumps (browser close in caller will end them anyway).
    console_pump.abort();
    runtime_pump.abort();
    exception_pump.abort();
    let _ = console_pump.await;
    let _ = runtime_pump.await;
    let _ = exception_pump.await;

    let page_errors = console_logs
        .lock()
        .await
        .iter()
        .filter(|line| {
            line.starts_with("[error]")
                || line.starts_with("[exception]")
                || line.contains("Failed to load resource")
        })
        .cloned()
        .collect();

    if !boot_log_found {
        bail!(
            "boot log {:?} not observed within {}s",
            BOOT_LOG_NEEDLE,
            args.timeout_secs
        );
    }
    if canvas_width == 0 || canvas_height == 0 {
        bail!(
            "canvas dimensions invalid: {}x{}",
            canvas_width,
            canvas_height
        );
    }

    Ok(RunOk {
        canvas_width,
        canvas_height,
        boot_log_found,
        header_actions,
        page_errors,
    })
}

/// Poll the base URL with HTTP GET via a raw TCP+HTTP/1.1 handshake. We
/// avoid pulling reqwest just for a liveness probe — the wrapper script
/// already does a curl loop before invoking us, so this is a belt-and-
/// suspenders check that yields a clear error.
async fn probe_server(url: &str, timeout: Duration) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let (host, port, path) = parse_url(url)?;
    let deadline = Instant::now() + timeout;
    loop {
        let attempt = async {
            let mut stream = TcpStream::connect((host.as_str(), port)).await?;
            let req = format!(
                "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(req.as_bytes()).await?;
            let mut buf = Vec::with_capacity(512);
            // Read up to first chunk; we just need the status line.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                stream.read_to_end(&mut buf),
            )
            .await;
            let head = String::from_utf8_lossy(&buf);
            if head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200") {
                anyhow::Ok(())
            } else {
                Err(anyhow!(
                    "unexpected response: {}",
                    head.lines().next().unwrap_or("<empty>")
                ))
            }
        }
        .await;

        match attempt {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() < deadline => {
                tracing::debug!("server probe pending: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e.context(format!("server probe {url} timed out"))),
        }
    }
}

fn parse_url(url: &str) -> Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("only http:// URLs supported, got {url}"))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rfind(':') {
        Some(i) => (
            hostport[..i].to_string(),
            hostport[i + 1..]
                .parse::<u16>()
                .with_context(|| format!("bad port in {url}"))?,
        ),
        None => (hostport.to_string(), 80),
    };
    Ok((host, port, path.to_string()))
}

fn tail(v: &[String], n: usize) -> Vec<String> {
    let start = v.len().saturating_sub(n);
    v[start..].to_vec()
}
