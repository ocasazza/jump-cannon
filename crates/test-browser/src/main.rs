//! Rust-driven browser regression suite.
//!
//! Asserts the bare minimum that future regression checks will build on:
//!
//!   1. The page at `--base-url` responds (HTTP 200).
//!   2. Headless Chromium launches with WebGPU flags and navigates.
//!   3. The boot log line `[jump-cannon-ui] boot` appears on the JS
//!      console within `--timeout-secs`.
//!   4. The static boot shell is outside and adjacent to the Dioxus mount,
//!      then hidden after mount.
//!   5. The graph canvas becomes render-ready and its header controls work.
//!   6. Nodes is a two-pane editor; Flat/Tags selection and content work.
//!   7. Unified Settings exposes four accessible, content-backed tabs.
//!   8. Filter is a repeatable, nested Boolean builder with live validation.
//!   9. The Sessions view switcher mounts the world workspace (Worlds panel,
//!      dock) against the embedded host and returns to the User view.
//!   10. Screenshots are saved for the Nodes editor, Filter builder, Sessions
//!       view, and workspace.
//!   11. Runtime importer switching: a second fixture graph-api (two-source
//!       Obsidian catalog + switch group) is mirrored/spawned in parallel;
//!       the scenario asserts the wire gate (403/200/404), the authorized
//!       selector + graph swap + sessionStorage persistence, the fresh-tab
//!       default, and the unauthorized note + stale-selection reset.
//!
//! Anything flaky (pixel brightness, motion deltas, click recovery) is
//! deliberately deferred. (The legacy egui-era Playwright suite that held
//! those checks was removed with the egui frontend — see git history.)

use std::path::{Path, PathBuf};
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

/// Keep a renderer-busy readiness probe shorter than chromiumoxide's fixed
/// 30-second command deadline. Linux software WebGPU may synchronously occupy
/// the renderer while it creates the device and pipelines; dropping a stalled
/// probe lets a later attempt observe the initialized page without weakening
/// the overall smoke-test deadline.
const READINESS_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// CDP's Log domain and Runtime console domain report severity differently.
/// Preserve native error/exception levels, and also catch Rust tracing's
/// `ERROR ...` records, which tracing-wasm emits through `console.log`.
fn browser_log_is_error(line: &str) -> bool {
    if line.starts_with("[error]") || line.starts_with("[exception]") {
        return true;
    }
    line.strip_prefix("[log]")
        .map(str::trim_start)
        .map(|message| message.trim_start_matches('"').replace("%c", ""))
        .map(|message| message.starts_with("ERROR "))
        .unwrap_or(false)
}

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

    /// Require the wrapper's stable Nodes editor fixtures and strict checks.
    #[arg(long)]
    fixtures_required: bool,
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
    pre_wasm_mount: Option<PreWasmMountCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_header_actions: Option<HeaderActionCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes_editor: Option<NodesEditorCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_tabs: Option<SettingsTabsCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter_builder: Option<FilterBuilderCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions_view: Option<SessionsViewCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    importer_switch: Option<ImporterSwitchCheck>,
    page_errors: Vec<String>,
    console_logs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PreWasmMountCheck {
    ok: bool,
    main_mount_found: bool,
    static_boot_found: bool,
    static_boot_inside_main: bool,
    static_boot_adjacent_sibling: bool,
    static_boot_hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
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
    action_count: usize,
    play_pause_found: bool,
    fit_found: bool,
    geometry_ok: bool,
    clicks_ok: bool,
    drag_started: bool,
    canvas_present: bool,
    render_ready: bool,
    secure_context: bool,
    webgpu_available: bool,
    node_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    actions: Vec<HeaderActionDetail>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct NodesEditorCheck {
    ok: bool,
    fixture_contract: bool,
    panel_visible: bool,
    horizontal_split: bool,
    controls_below_header: bool,
    controls_hit_test: bool,
    tiling_geometry: bool,
    sidebar_width: f64,
    main_width: f64,
    flat_default: bool,
    selected_content_loaded: bool,
    selection_persisted: bool,
    exact_tag_groups: bool,
    hierarchical_tag_paths: bool,
    untagged_group: bool,
    flat_active_count: usize,
    schema_core_keys: bool,
    search_schema_generic: bool,
    #[serde(default)]
    tag_mode_ready: bool,
    #[serde(default)]
    generic_groups_exact: bool,
    #[serde(default)]
    fixture_groups_exact: bool,
    #[serde(default)]
    fixture_id: String,
    #[serde(default)]
    untagged_ids: Vec<String>,
    #[serde(default)]
    editor_group_ids: Vec<String>,
    #[serde(default)]
    shared_group_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SettingsTabsCheck {
    ok: bool,
    labels: Vec<String>,
    content_panels: Vec<String>,
    aria_contract: bool,
    keyboard_contract: bool,
    controls_hit_test: bool,
    importer_catalog: bool,
    importer_read_only: bool,
    importer_switch_posture: bool,
    legacy_panels_absent: bool,
    graph_restored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FilterBuilderCheck {
    ok: bool,
    panel_visible: bool,
    independent_search_rules: bool,
    field_rules: bool,
    all_count: usize,
    any_count: usize,
    mode_counts: bool,
    inline_diagnostic: bool,
    last_valid_state: bool,
    accessible_reorder: bool,
    graph_restored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SessionsViewCheck {
    ok: bool,
    switch_found: bool,
    sessions_active: bool,
    worlds_panel: bool,
    worlds_empty_state: bool,
    dock_present: bool,
    user_restored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Runtime per-viewer importer switching, exercised against a second,
/// self-hosted fixture graph-api (the main fixture has no switch group, so
/// the selector-absent contract lives in `settings_tabs`). The scenario
/// spawns `graph-api` from PATH with `JUMP_CANNON_IMPORTER_SWITCH_GROUP` and
/// a two-source Obsidian catalog, mirrors the app dist from `--base-url`, and
/// simulates the authenticating proxy with `Network.setExtraHTTPHeaders`.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ImporterSwitchCheck {
    ok: bool,
    /// True when no `graph-api` binary was available to host the second
    /// fixture (e.g. a direct run against a remote base URL). Skipped runs
    /// never fail the suite; the `just test browser-rust` wrapper always has
    /// the binary on PATH, so the gate exercises the full contract.
    #[serde(default)]
    skipped: bool,
    wire_forbidden: bool,
    wire_authorized: bool,
    wire_unknown: bool,
    selector_visible: bool,
    switch_swaps_nodes: bool,
    session_persists_reload: bool,
    switch_back_restores_default: bool,
    fresh_tab_default: bool,
    denied_note: bool,
    stale_reset_recovers: bool,
    /// Authorized viewer currently viewing the alternate: the policy reset
    /// affordance must be visible and clicking it must clear the session
    /// selection and restore the deployment default. Covers the case where
    /// `stale_reset_recovers` does not (viewer still has the group).
    viewing_non_default_reset: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl ImporterSwitchCheck {
    fn skipped() -> Self {
        Self {
            ok: true,
            skipped: true,
            wire_forbidden: false,
            wire_authorized: false,
            wire_unknown: false,
            selector_visible: false,
            switch_swaps_nodes: false,
            session_persists_reload: false,
            switch_back_restores_default: false,
            fresh_tab_default: false,
            denied_note: false,
            stale_reset_recovers: false,
            viewing_non_default_reset: false,
            reason: Some("graph-api binary not on PATH; scenario skipped".to_string()),
        }
    }

    fn failed(reason: String) -> Self {
        Self {
            ok: false,
            skipped: false,
            reason: Some(reason),
            ..Self::skipped()
        }
    }
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

    let result = tokio::time::timeout(
        Duration::from_secs(args.timeout_secs),
        run(&args, console_logs.clone()),
    )
    .await
    .unwrap_or_else(|_| {
        Err(anyhow!(
            "browser smoke exceeded the overall {}s timeout",
            args.timeout_secs
        ))
    });
    let duration_ms = started.elapsed().as_millis();

    let logs = console_logs.lock().await.clone();
    let captured_page_errors: Vec<String> = logs
        .iter()
        .filter(|line| browser_log_is_error(line))
        .cloned()
        .collect();
    let (
        ok,
        reason,
        canvas_width,
        canvas_height,
        boot_log_found,
        pre_wasm_mount,
        header_actions,
        nodes_editor,
        settings_tabs,
        filter_builder,
        sessions_view,
        importer_switch,
        page_errors,
    ) = match &result {
        Ok(o) => {
            let ok = o.boot_log_found
                && o.canvas_width > 0
                && o.canvas_height > 0
                && o.pre_wasm_mount.ok
                && o.header_actions.ok
                && o.nodes_editor.ok
                && o.settings_tabs.ok
                && o.filter_builder.ok
                && o.sessions_view.ok
                && o.importer_switch.ok
                && captured_page_errors.is_empty();
            let reason = if !o.boot_log_found {
                Some(format!("boot log {BOOT_LOG_NEEDLE:?} was not observed"))
            } else if o.canvas_width == 0 || o.canvas_height == 0 {
                Some(format!(
                    "canvas dimensions invalid: {}x{}",
                    o.canvas_width, o.canvas_height
                ))
            } else if !o.pre_wasm_mount.ok {
                o.pre_wasm_mount.reason.clone()
            } else if !o.header_actions.ok {
                o.header_actions.reason.clone()
            } else if !o.nodes_editor.ok {
                o.nodes_editor.reason.clone()
            } else if !o.settings_tabs.ok {
                o.settings_tabs.reason.clone()
            } else if !o.filter_builder.ok {
                o.filter_builder.reason.clone()
            } else if !o.sessions_view.ok {
                o.sessions_view.reason.clone()
            } else if !o.importer_switch.ok {
                o.importer_switch.reason.clone()
            } else if !captured_page_errors.is_empty() {
                Some(format!(
                    "browser emitted {} console error(s) or unhandled exception(s)",
                    captured_page_errors.len()
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
                Some(o.pre_wasm_mount.clone()),
                Some(o.header_actions.clone()),
                Some(o.nodes_editor.clone()),
                Some(o.settings_tabs.clone()),
                Some(o.filter_builder.clone()),
                Some(o.sessions_view.clone()),
                Some(o.importer_switch.clone()),
                captured_page_errors.clone(),
            )
        }
        Err(e) => (
            false,
            Some(format!("{e:#}")),
            0,
            0,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            captured_page_errors.clone(),
        ),
    };

    let report = Report {
        ok,
        base_url: args.base_url.clone(),
        canvas_width,
        canvas_height,
        boot_log_found,
        duration_ms,
        reason: reason.clone(),
        pre_wasm_mount,
        graph_header_actions: header_actions,
        nodes_editor,
        settings_tabs,
        filter_builder,
        sessions_view,
        importer_switch,
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
    pre_wasm_mount: PreWasmMountCheck,
    header_actions: HeaderActionCheck,
    nodes_editor: NodesEditorCheck,
    settings_tabs: SettingsTabsCheck,
    filter_builder: FilterBuilderCheck,
    sessions_view: SessionsViewCheck,
    importer_switch: ImporterSwitchCheck,
}

fn chromium_args() -> Vec<&'static str> {
    // chromiumoxide models arguments as keys and adds the `--` prefix when it
    // builds the command line. Supplying CLI-form strings here would turn
    // `--foo` into `----foo`, which Chromium silently ignores.
    vec![
        "enable-unsafe-webgpu",
        "disable-dev-shm-usage",
        "disable-gpu-sandbox",
    ]
}

async fn run(args: &Args, console_logs: Arc<Mutex<Vec<String>>>) -> Result<RunOk> {
    // ---- 1. server reachability ------------------------------------------
    let probe_url = args.base_url.trim_end_matches('/').to_string() + "/";
    if raw_http_probe_required(&probe_url)? {
        probe_server(&probe_url, Duration::from_secs(args.timeout_secs.min(30))).await?;
    }

    // ---- 2. launch chromium ----------------------------------------------
    let mut config = BrowserConfig::builder()
        .chrome_executable(&args.chromium)
        .args(chromium_args())
        // Panel Kit intentionally persists workspace and per-panel choices.
        // Keep regression defaults deterministic and discard state on exit.
        .incognito()
        // The flake wrapper gives each invocation a unique temporary out_dir.
        // Keep Chrome's ProcessSingleton/profile lock scoped to that run too,
        // so concurrent browser gates cannot attach to or evict one another.
        .user_data_dir(args.out_dir.join("chromium-profile"))
        // This is an isolated automation browser. Use chromiumoxide's typed
        // switch so rootful container callers cannot accidentally omit it.
        .no_sandbox()
        // The fixed browser window is enough for these geometry assertions.
        // Disabling viewport emulation avoids a post-navigation
        // Emulation.setDeviceMetricsOverride command that can time out while
        // Linux software WebGPU is saturating the render thread.
        .viewport(None)
        .window_size(1280, 800);
    // WebGPU is restricted to secure contexts. Cluster smoke tests use an
    // internal plain-HTTP Service, so grant only that explicitly requested
    // test origin secure-context treatment in this disposable browser.
    if args.base_url.starts_with("http://") {
        config = config.arg((
            "unsafely-treat-insecure-origin-as-secure",
            args.base_url.trim_end_matches('/'),
        ));
    }
    // Headless Linux uses the Nix-provided lavapipe Vulkan adapter. Valued
    // arguments must be tuples so chromiumoxide merges `enable-features`
    // with its defaults instead of emitting duplicate switches.
    if cfg!(target_os = "linux") {
        config = config
            .arg(("enable-features", "Vulkan"))
            .arg(("use-angle", "vulkan"))
            .arg(("use-gl", "angle"))
            // Unified headless Chrome has no display swapchain. Keep Dawn's
            // Vulkan adapter/compute path, but present through its headless
            // blit path so the first surface frame cannot block Runtime/CDP.
            .arg("disable-vulkan-surface");
    }
    let config = config.build().map_err(|e| anyhow!("BrowserConfig: {e}"))?;

    let (mut browser, mut handler) = Browser::launch(config).await.context("Browser::launch")?;

    // The CDP handler must be driven; spawn a task that polls it.
    let handler_task = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if let Err(error) = event {
                tracing::error!("CDP handler failed: {error}");
                break;
            }
        }
    });

    let outcome = drive_page(&browser, args, console_logs).await;

    // Best-effort browser teardown.
    let close_ok = matches!(
        tokio::time::timeout(Duration::from_secs(5), browser.close()).await,
        Ok(Ok(_))
    );
    if !close_ok {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            match browser.kill().await {
                Some(result) => result,
                None => Ok(()),
            }
        })
        .await;
    }
    drop(browser);
    handler_task.abort();
    let _ = handler_task.await;

    outcome
}

async fn drive_page(
    browser: &Browser,
    args: &Args,
    console_logs: Arc<Mutex<Vec<String>>>,
) -> Result<RunOk> {
    use chromiumoxide::cdp::browser_protocol::page::EventLifecycleEvent;

    let page = browser.new_page("about:blank").await.context("new_page")?;

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
    let _ = page.event_listener::<EventLifecycleEvent>().await.ok();

    let logs_a = console_logs.clone();
    let console_pump = tokio::spawn(async move {
        while let Some(ev) = console_events.next().await {
            let line = format!("[{}] {}", ev.entry.level.as_ref(), ev.entry.text);
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
    // chromiumoxide special-cases Page.navigate and waits for the browser's
    // page-load lifecycle. Headless software WebGPU can keep that lifecycle
    // pending even after the Dioxus app boots, causing the command's 30s
    // deadline to fail. Schedule navigation through Runtime.evaluate instead;
    // the explicit boot/render checks below are the readiness contract.
    let target_json = serde_json::to_string(&target).context("encode navigation target")?;
    page.evaluate(format!(
        "setTimeout(() => window.location.replace({target_json}), 0)"
    ))
    .await
    .context("schedule navigation")?;

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

    // ---- 3b. importer-switch fixture setup runs in parallel ---------------
    // Mirroring the app dist (raw HTTP) and booting the second fixture
    // graph-api overlaps the main-page checks; the browser scenario itself
    // runs sequentially at the end.
    let switch_origin = args.base_url.trim_end_matches('/').to_string();
    let switch_setup = tokio::spawn(async move { setup_switch_fixture(&switch_origin).await });

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
        const nodeCount = Number(canvas?.dataset.nodeCount || 0);
        return Boolean(
          panel && header && canvas && actions?.length >= 2 &&
          canvas.dataset.renderReady === 'true' && nodeCount > 0
        );
    })()"#;
    let graph_deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let graph_wait_started = Instant::now();
    let mut graph_probe_attempt = 0_u32;
    let mut last_graph_probe_error = None;
    loop {
        let remaining = graph_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let detail = last_graph_probe_error
                .as_deref()
                .unwrap_or("the page remained responsive but not render-ready");
            bail!(
                "graph did not become render-ready within {}s after {} probe(s): {detail}",
                args.timeout_secs,
                graph_probe_attempt
            );
        }

        graph_probe_attempt += 1;
        let probe_timeout = READINESS_PROBE_TIMEOUT.min(remaining);
        let probe = tokio::time::timeout(probe_timeout, page.evaluate(graph_ready_js)).await;
        match probe {
            Ok(Ok(value)) => {
                let ready: bool = value.into_value().context("decode graph readiness probe")?;
                if ready {
                    tracing::info!(
                        attempt = graph_probe_attempt,
                        elapsed_ms = graph_wait_started.elapsed().as_millis(),
                        "graph became render-ready"
                    );
                    break;
                }
                last_graph_probe_error = None;
            }
            Ok(Err(error)) => {
                let detail = format!("Runtime.evaluate failed: {error:#}");
                tracing::warn!(
                    attempt = graph_probe_attempt,
                    elapsed_ms = graph_wait_started.elapsed().as_millis(),
                    "graph readiness probe failed; retrying: {detail}"
                );
                last_graph_probe_error = Some(detail);
            }
            Err(_) => {
                let detail = format!(
                    "Runtime.evaluate exceeded the {}s per-probe timeout",
                    probe_timeout.as_secs()
                );
                tracing::warn!(
                    attempt = graph_probe_attempt,
                    elapsed_ms = graph_wait_started.elapsed().as_millis(),
                    "graph readiness probe stalled; retrying: {detail}"
                );
                last_graph_probe_error = Some(detail);
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // ---- 4b. static pre-WASM shell hands off without sharing #main -------
    // The static shell must remain outside Dioxus's mount node. Otherwise the
    // first virtual-DOM diff can try to reconcile server-authored children and
    // panic with `invalid key`. This runs after the render-ready wait so the
    // Dioxus commit (and the shell-hiding selector) have already settled.
    let pre_wasm_mount_js = r#"(async () => {
        await new Promise((resolve) => requestAnimationFrame(() =>
          requestAnimationFrame(resolve)
        ));
        const mainMount = document.querySelector('#main');
        const staticBoot = document.querySelector('[data-panel-kit-static-boot]');
        const staticBootInsideMain = Boolean(
          mainMount && staticBoot && mainMount.contains(staticBoot)
        );
        const staticBootAdjacentSibling = Boolean(
          mainMount && staticBoot && mainMount.nextElementSibling === staticBoot
        );
        const staticBootHidden = Boolean(
          staticBoot && getComputedStyle(staticBoot).display === 'none'
        );
        const failures = [];
        if (!mainMount) failures.push('#main Dioxus mount missing');
        if (!staticBoot) failures.push('static pre-WASM boot shell missing');
        if (staticBootInsideMain) {
          failures.push('static pre-WASM boot shell is inside #main');
        }
        if (mainMount && staticBoot && !staticBootAdjacentSibling) {
          failures.push('static pre-WASM boot shell is not the adjacent sibling after #main');
        }
        if (staticBoot && !staticBootHidden) {
          failures.push('static pre-WASM boot shell still visible after Dioxus mount');
        }
        return {
          ok: failures.length === 0,
          main_mount_found: Boolean(mainMount),
          static_boot_found: Boolean(staticBoot),
          static_boot_inside_main: staticBootInsideMain,
          static_boot_adjacent_sibling: staticBootAdjacentSibling,
          static_boot_hidden: staticBootHidden,
          reason: failures.length ? failures.join('; ') : null,
        };
    })()"#;
    let pre_wasm_mount_value: serde_json::Value =
        page.evaluate(pre_wasm_mount_js).await?.into_value()?;
    let pre_wasm_mount: PreWasmMountCheck = serde_json::from_value(pre_wasm_mount_value)
        .context("decode pre-WASM mount invariant result")?;
    if !pre_wasm_mount.ok {
        // Stop here: a broken mount handoff would only obscure every later
        // check, and the bail reason carries the structured failures.
        let detail = pre_wasm_mount
            .reason
            .clone()
            .unwrap_or_else(|| "unknown mount invariant failure".to_string());
        bail!("pre-WASM mount invariant failed: {detail}");
    }

    // ---- 5. Nodes is an editor-style navigator + focused-content surface --
    // Exercise real buttons and wait on the same DOM state a user sees. The
    // wrapper seeds stable fixtures so this contract is independent of edits
    // to the living documentation corpus.
    let nodes_editor_js = r#"(async () => {
        const waitFor = async (predicate, timeoutMs = 6000) => {
          const deadline = performance.now() + timeoutMs;
          while (performance.now() < deadline) {
            const value = predicate();
            if (value) return value;
            await new Promise((resolve) => setTimeout(resolve, 50));
          }
          return null;
        };
        const failures = [];
        const editor = await waitFor(() => document.querySelector('[data-testid="nodes-editor"]'));
        const panel = editor?.closest('section.panel-nodes');
        const header = panel?.querySelector(':scope > header.panel-head');
        const sidebar = editor?.querySelector('[data-testid="node-sidebar"]');
        const main = editor?.querySelector('[data-testid="node-main"]');
        const search = editor?.querySelector('input.nodes-search');
        const flat = editor?.querySelector('[data-node-list-mode="flat"]');
        const tags = editor?.querySelector('[data-node-list-mode="tags"]');
        const editorRect = editor?.getBoundingClientRect();
        const headerRect = header?.getBoundingClientRect();
        const sidebarRect = sidebar?.getBoundingClientRect();
        const mainRect = main?.getBoundingClientRect();
        const panelVisible = Boolean(
          panel && editorRect && editorRect.width > 0 && editorRect.height > 0
        );
        const horizontalSplit = Boolean(
          sidebarRect && mainRect &&
          sidebarRect.width > 0 && mainRect.width > sidebarRect.width &&
          mainRect.left >= sidebarRect.right - 1
        );
        const controls = [search, flat, tags].filter(Boolean);
        const controlsBelowHeader = Boolean(
          headerRect && editorRect && controls.length === 3 &&
          editorRect.top >= headerRect.bottom - 1 &&
          controls.every((control) => control.getBoundingClientRect().top >= headerRect.bottom - 1)
        );
        const controlsHitTest = controls.length === 3 && controls.every((control) => {
          const rect = control.getBoundingClientRect();
          const hit = document.elementFromPoint(
            rect.left + rect.width / 2,
            rect.top + rect.height / 2
          );
          return hit === control || control.contains(hit);
        });

        const flatDefault = flat?.getAttribute('aria-pressed') === 'true';
        const schemaCoreKeys = Boolean(await waitFor(() => {
          const keys = [...(editor?.querySelectorAll('.search-schema-key') || [])]
            .map((element) => (element.textContent || '').trim());
          return ['id:', 'title:', 'tags:'].every((key) => keys.includes(key));
        }));
        const searchSchemaGeneric = Boolean(await waitFor(() => {
          const schema = editor?.querySelector('.search-schema');
          const label = schema?.querySelector('[data-search-schema-label]');
          return (label?.textContent || '').trim() === 'Search fields' &&
            schema?.getAttribute('aria-label') === 'Search fields from active importer schema' &&
            Boolean(schema?.getAttribute('data-search-schema-source'));
        }));

        const strictFixture = sidebar?.querySelector(
          '[data-node-id="Node Editor Fixture"], [data-node-id$=":Node Editor Fixture"]'
        );
        const fixtureContract = Boolean(strictFixture);
        const fixture = strictFixture || sidebar?.querySelector('[data-node-id]');
        const fixtureId = fixture?.getAttribute('data-node-id') || '';
        fixture?.click();
        const selectedContentLoaded = Boolean(await waitFor(() =>
          main?.querySelector('[data-focused-node]')?.getAttribute('data-focused-node') === fixtureId &&
          (!fixtureContract ||
            (main?.textContent || '').includes('BROWSER_NODE_EDITOR_SENTINEL'))
        ));

        tags?.click();
        const tagModeReady = Boolean(await waitFor(() =>
          tags?.getAttribute('aria-pressed') === 'true' &&
          editor?.querySelector('[data-tag]')
        ));
        const groups = [...(editor?.querySelectorAll('[data-tag-kind="exact"]') || [])];
        const groupNamed = (tag) => groups.find(
          (group) => group.getAttribute('data-tag') === tag
        );
        const untaggedGroup = editor?.querySelector('[data-synthetic-group="untagged"]');
        const expand = (group) => {
          const summary = group?.querySelector('.nodes-tag-summary');
          if (summary?.getAttribute('aria-expanded') !== 'true') summary?.click();
          return group;
        };
        const groupAtPath = (path) => [...(editor?.querySelectorAll('[data-tag-path]') || [])]
          .find((group) => group.getAttribute('data-tag-path') === path);
        const expandPath = async (path) => {
          const segments = path.split('/');
          let prefix = '';
          let group = null;
          for (const segment of segments) {
            prefix = prefix ? `${prefix}/${segment}` : segment;
            group = await waitFor(() => groupAtPath(prefix));
            if (!group) return null;
            expand(group);
          }
          return group;
        };
        const fooLeaf = fixtureContract ? await expandPath('foo/bar/baz') : null;
        const beeLeaf = fixtureContract ? await expandPath('bee/bop/baz') : null;
        if (fixtureContract) {
          await waitFor(() => [fooLeaf, beeLeaf].every(
            (group) => group?.querySelector('[data-node-id="Node Editor Fixture"], [data-node-id$=":Node Editor Fixture"]')
          ));
        }
        const groupsToExercise = fixtureContract
          ? [groupNamed('browser-editor'), groupNamed('browser-shared')].filter(Boolean)
          : groups.slice(0, 2);
        groupsToExercise.forEach(expand);
        expand(untaggedGroup);
        await waitFor(() => groupsToExercise.every(
          (group) => group.querySelector('[data-node-id]')
        ));
        const genericGroupsExact = groupsToExercise.length > 0 && groupsToExercise.every(
          (group) => {
            const ids = [...group.querySelectorAll('[data-node-id]')]
              .map((node) => node.getAttribute('data-node-id'));
            return ids.length > 0 && new Set(ids).size === ids.length;
          }
        );
        const editorGroup = groupNamed('browser-editor');
        const sharedGroup = groupNamed('browser-shared');
        const fixtureGroupsExact = Boolean(
          editorGroup && sharedGroup &&
          [...editorGroup.querySelectorAll('[data-node-id]')]
            .filter((node) => node.getAttribute('data-node-id') === fixtureId).length === 1 &&
          [...sharedGroup.querySelectorAll('[data-node-id]')]
            .filter((node) => node.getAttribute('data-node-id') === fixtureId).length === 1 &&
          [...sharedGroup.querySelectorAll('[data-node-id]')]
            .some((node) => {
              const id = node.getAttribute('data-node-id');
              return id === 'Node Shared Fixture' || id?.endsWith(':Node Shared Fixture');
            })
        );
        const exactTagGroups = tagModeReady && genericGroupsExact &&
          (!fixtureContract || fixtureGroupsExact);
        const hierarchicalTagPaths = !fixtureContract || Boolean(
          fooLeaf && beeLeaf &&
          fooLeaf.getAttribute('data-tag-segment') === 'baz' &&
          beeLeaf.getAttribute('data-tag-segment') === 'baz' &&
          [...fooLeaf.querySelectorAll('[data-node-id="Node Editor Fixture"], [data-node-id$=":Node Editor Fixture"]')].length === 1 &&
          [...beeLeaf.querySelectorAll('[data-node-id="Node Editor Fixture"], [data-node-id$=":Node Editor Fixture"]')].length === 1
        );
        const untaggedGroupPresent = Boolean(
          untaggedGroup?.querySelector('[data-node-id]')
        );
        const fixtureUntagged = Boolean(
          [...(untaggedGroup?.querySelectorAll('[data-node-id]') || [])]
            .some((node) => {
              const id = node.getAttribute('data-node-id');
              return id === 'Node Untagged Fixture' || id?.endsWith(':Node Untagged Fixture');
            })
        );
        const selectionPersisted = Boolean(
          main?.querySelector('[data-focused-node]')?.getAttribute('data-focused-node') === fixtureId &&
          (!fixtureContract ||
            (main?.textContent || '').includes('BROWSER_NODE_EDITOR_SENTINEL'))
        );

        flat?.click();
        await waitFor(() => flat?.getAttribute('aria-pressed') === 'true');
        const flatActiveCount = [...(sidebar?.querySelectorAll(
          '[data-node-id][aria-current="page"]'
        ) || [])].filter(
          (node) => node.getAttribute('data-node-id') === fixtureId
        ).length;

        // Leave the hierarchy visible for the screenshot while proving the
        // selection survives the round trip through both navigator modes.
        tags?.click();
        await waitFor(() => tags?.getAttribute('aria-pressed') === 'true');

        // The editor must remain usable in Panel Kit's other desktop mode.
        // A too-small default tile was the second half of the reported visual
        // regression, so check both size and the left-nav/content relationship.
        const workspace = panel?.closest('.ws');
        const modeToggle = panel?.querySelector('.light.mode');
        let tilingGeometry = false;
        if (workspace && modeToggle && header && search && sidebar && main) {
          modeToggle.click();
          const tiled = await waitFor(() => workspace.classList.contains('tiling'));
          if (tiled) {
            const tiledPanel = panel.getBoundingClientRect();
            const tiledHeader = header.getBoundingClientRect();
            const tiledSearch = search.getBoundingClientRect();
            const tiledSidebar = sidebar.getBoundingClientRect();
            const tiledMain = main.getBoundingClientRect();
            tilingGeometry = Boolean(
              tiledPanel.width >= 480 && tiledPanel.height >= 450 &&
              tiledSearch.top >= tiledHeader.bottom - 1 &&
              tiledSidebar.width > 0 && tiledMain.width > tiledSidebar.width &&
              tiledMain.left >= tiledSidebar.right - 1
            );
          }
          modeToggle.click();
          await waitFor(() => workspace.classList.contains('floating'));
        }

        if (!panelVisible) failures.push('Nodes editor panel missing or hidden');
        if (!horizontalSplit) failures.push('Nodes navigator is not left of a wider content pane');
        if (!controlsBelowHeader) failures.push('Nodes controls overlap the panel header');
        if (!controlsHitTest) failures.push('Nodes controls are obscured from pointer input');
        if (!tilingGeometry) failures.push('Nodes tile is too small for the editor layout');
        if (!flatDefault) failures.push('Flat navigator is not the fresh-layout default');
        if (!schemaCoreKeys) failures.push('core importer search keys missing');
        if (!searchSchemaGeneric) failures.push('search field label is importer-specific');
        if (!selectedContentLoaded) failures.push('selected node content did not load');
        if (!selectionPersisted) failures.push('selection/content did not survive Tags mode');
        if (!exactTagGroups) failures.push('exact multi-tag grouping is incorrect');
        if (!hierarchicalTagPaths) failures.push('required tag paths did not render as nested groups');
        if (fixtureContract && (!untaggedGroupPresent || !fixtureUntagged)) {
          failures.push('synthetic untagged group or fixture missing');
        }
        if (flatActiveCount !== 1) failures.push('Flat mode did not expose exactly one active row');
        return {
          ok: failures.length === 0,
          fixture_contract: fixtureContract,
          panel_visible: panelVisible,
          horizontal_split: horizontalSplit,
          controls_below_header: controlsBelowHeader,
          controls_hit_test: controlsHitTest,
          tiling_geometry: tilingGeometry,
          sidebar_width: sidebarRect?.width || 0,
          main_width: mainRect?.width || 0,
          flat_default: flatDefault,
          selected_content_loaded: selectedContentLoaded,
          selection_persisted: selectionPersisted,
          exact_tag_groups: Boolean(exactTagGroups),
          hierarchical_tag_paths: hierarchicalTagPaths,
          untagged_group: untaggedGroupPresent,
          flat_active_count: flatActiveCount,
          schema_core_keys: schemaCoreKeys,
          search_schema_generic: searchSchemaGeneric,
          tag_mode_ready: Boolean(tagModeReady),
          generic_groups_exact: Boolean(genericGroupsExact),
          fixture_groups_exact: Boolean(fixtureGroupsExact),
          fixture_id: fixtureId,
          untagged_ids: [...(untaggedGroup?.querySelectorAll('[data-node-id]') || [])]
            .map((node) => node.getAttribute('data-node-id')).slice(0, 8),
          editor_group_ids: [...(editorGroup?.querySelectorAll('[data-node-id]') || [])]
            .map((node) => node.getAttribute('data-node-id')),
          shared_group_ids: [...(sharedGroup?.querySelectorAll('[data-node-id]') || [])]
            .map((node) => node.getAttribute('data-node-id')),
          reason: failures.length ? failures.join('; ') : null,
        };
    })()"#;
    let nodes_editor_value: serde_json::Value =
        page.evaluate(nodes_editor_js).await?.into_value()?;
    let mut nodes_editor: NodesEditorCheck = serde_json::from_value(nodes_editor_value)
        .context("decode Nodes editor regression result")?;
    if args.fixtures_required && !nodes_editor.fixture_contract {
        nodes_editor.ok = false;
        nodes_editor.reason =
            Some("required Nodes editor fixtures were not imported into the navigator".to_string());
    }

    let nodes_png = page
        .screenshot(CaptureScreenshotParams::builder().build())
        .await
        .context("Nodes editor screenshot")?;
    let nodes_bytes = if nodes_png.first() == Some(&0x89) {
        nodes_png
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(&nodes_png)
            .unwrap_or(nodes_png)
    };
    let nodes_shot_path = args.out_dir.join("nodes-editor.png");
    tokio::fs::write(&nodes_shot_path, nodes_bytes).await?;
    tracing::info!("wrote screenshot {}", nodes_shot_path.display());

    // ---- 6. Unified Settings exposes five accessible, real tabs ---------
    // Maximize Settings so every tab is both visible and pointer-hit-testable,
    // exercise each delegated panel, then restore the workspace. Restoring
    // also proves that the Graph canvas has one reliable remount owner.
    let settings_tabs_js = r#"(async () => {
        const waitFor = async (predicate, timeoutMs = 10000) => {
          const deadline = performance.now() + timeoutMs;
          while (performance.now() < deadline) {
            const value = predicate();
            if (value) return value;
            await new Promise((resolve) => setTimeout(resolve, 50));
          }
          return null;
        };
        const failures = [];
        const expected = [
          ['Connection', 'connection', 'input[aria-label="Graph API server URL"]'],
          ['Importers', 'importers', '.importer-card[data-source-id="lavender-ingest-okf"]'],
          ['Layout', 'layout', '.lay'],
          ['Appearance', 'appearance', '.sty'],
          ['Camera', 'camera', '.cam'],
        ];
        const initialPanel = document.querySelector('section.panel-settings');
        const maximize = initialPanel?.querySelector(
          ':scope > header.panel-head .light.max'
        );
        maximize?.click();
        // Panel Kit remounts the sole visible panel when maximizing. Reacquire
        // it before checking geometry or driving its Dioxus event handlers.
        const panel = await waitFor(() => {
          const candidate = document.querySelector('section.panel-settings');
          const rect = candidate?.getBoundingClientRect();
          return document.querySelector('.ws.maxed') &&
            rect?.width > 900 && rect?.height > 400 && candidate;
        });
        const maximized = Boolean(panel);

        const tabs = [...(panel?.querySelectorAll('[role="tab"]') || [])];
        const labels = tabs.map((tab) => (tab.textContent || '').trim());
        const contentPanels = [];
        let ariaContract = maximized && labels.length === expected.length &&
          expected.every(([label], index) => labels[index] === label);
        let controlsHitTest = maximized && tabs.length === expected.length;
        let importerCatalog = false;
        let importerReadOnly = false;
        let importerSwitchPosture = false;

        for (const [label, slug, selector] of expected) {
          const tab = tabs.find((candidate) => (candidate.textContent || '').trim() === label);
          tab?.click();
          const content = await waitFor(() => {
            const selected = panel?.querySelector(`[role="tab"][aria-selected="true"]`);
            const tabpanel = panel?.querySelector('[role="tabpanel"]');
            return selected === tab &&
              tabpanel?.id === `settings-panel-${slug}` &&
              tabpanel?.querySelector(selector) && tabpanel;
          });
          if (content) contentPanels.push(slug);

          if (label === 'Importers' && content) {
            const text = (selector) =>
              (content.querySelector(selector)?.textContent || '').trim();
            const lavender = content.querySelector(
              '.importer-card[data-source-id="lavender-ingest-okf"]'
            );
            const lavenderText = (selector) =>
              (lavender?.querySelector(selector)?.textContent || '').trim();
            const policy = text('.importer-policy');
            const switchState = content.querySelector('.importers-view')
              ?.getAttribute('data-runtime-switch');
            const switchControls = content.querySelectorAll('.importer-switch-btn');
            const selectedProfile = text('[data-field="selected-profile"]');
            const activeKind = text('[data-field="active-kind"]');
            const knownKinds = new Set([
              'obsidian', 'tvix', 'generate', 'kubernetes', 'okf', 'pest',
              'github', 'world'
            ]);
            const cards = [...content.querySelectorAll('.importer-card[data-source-id]')];
            const selectedCards = cards.filter(
              (card) => card.getAttribute('data-selected') === 'true'
            );
            const activeCards = cards.filter(
              (card) => card.getAttribute('data-active') === 'true'
            );
            const selectionMatches = selectedProfile === 'none'
              ? knownKinds.has(activeKind) && selectedCards.length === 0 && activeCards.length === 0
              : knownKinds.has(activeKind) &&
                selectedCards.length === 1 && activeCards.length === 1 &&
                selectedCards[0] === activeCards[0] &&
                selectedCards[0].getAttribute('data-source-id') === selectedProfile &&
                selectedCards[0].getAttribute('data-kind') === activeKind;
            // The policy note follows the per-viewer runtime-switch posture:
            // a disabled deployment requires a rollout, an authorized viewer
            // gets runtime viewing, and a denied viewer gets the
            // group-required note. All three remain deployment-managed.
            const policyByState = {
              disabled: /rollout is required/i,
              enabled: /Runtime viewing is enabled/i,
              denied: /Switching requires NetBird group/i,
            };
            importerCatalog = Boolean(
              content.querySelector('.importers-view[data-activation="helm_rollout"]') &&
              lavender &&
              selectionMatches &&
              text('[data-field="active-importer-id"]') &&
              /Configured by Helm/i.test(policy) &&
              Boolean(policyByState[switchState]?.test(policy))
            );
            importerReadOnly = Boolean(
              lavender?.querySelector('.importer-badge.read-only') &&
              lavenderText('[data-field="consumer-volume"]') === 'lavender-okf-repository' &&
              lavenderText('[data-field="consumer-claim"]') === 'lavender-okf-shared' &&
              lavenderText('[data-field="consumer-mount"]') ===
                '/var/lib/lavender/okf-repository' &&
              lavenderText('[data-field="consumer-input"]') ===
                '/var/lib/lavender/okf-repository/okf' &&
              lavenderText('[data-field="consumer-access"]') === 'read-only' &&
              lavenderText('[data-field="producer-default-claim"]') ===
                'lavender-ingest-okf' &&
              lavenderText('[data-field="producer-repository-root"]') ===
                '/data/okf-repository' &&
              lavenderText('[data-field="producer-workflow-input"]') ===
                '/data/okf-repository/okf' &&
              lavenderText('[data-field="producer-existing-claim-value-path"]') ===
                'okf.persistence.existingClaim' &&
              lavenderText('[data-field="producer-existing-claim-value"]') ===
                'lavender-okf-shared'
            );
            const lavenderDescription = lavenderText('.importer-description');
            importerReadOnly &&= /deployment-provisioned RWX/i.test(lavenderDescription) &&
              /same namespace/i.test(lavenderDescription) &&
              /<release>-okf/.test(lavenderDescription) &&
              /UID\/GID 10001/i.test(lavenderDescription);
            // Switch controls must exist exactly when this viewer is
            // authorized: absent for disabled deployments (the local fixture)
            // and for denied viewers (the deployment's unprivileged browser
            // identity), present for enabled viewers. The full authorized
            // selector contract is exercised by the importer_switch scenario
            // against a second fixture server.
            importerSwitchPosture = Boolean(
              ((switchState === 'disabled' || switchState === 'denied') &&
                switchControls.length === 0) ||
              (switchState === 'enabled' && switchControls.length > 0)
            );
          }

          const selectedTabs = tabs.filter(
            (candidate) => candidate.getAttribute('aria-selected') === 'true'
          );
          const controlled = tab?.getAttribute('aria-controls') === `settings-panel-${slug}`;
          const rovingTabindex = selectedTabs.length === 1 && tabs.every((candidate) =>
            candidate.getAttribute('tabindex') === (candidate === tab ? '0' : '-1')
          );
          ariaContract &&= Boolean(content && controlled && rovingTabindex);

          if (tab) {
            const rect = tab.getBoundingClientRect();
            const hit = document.elementFromPoint(
              rect.left + rect.width / 2,
              rect.top + rect.height / 2
            );
            controlsHitTest &&= hit === tab || tab.contains(hit);
          } else {
            controlsHitTest = false;
          }
        }

        const keyboardStep = async (fromLabel, key, toLabel) => {
          const currentTabs = [...(panel?.querySelectorAll('[role="tab"]') || [])];
          const from = currentTabs.find(
            (tab) => (tab.textContent || '').trim() === fromLabel
          );
          const to = currentTabs.find(
            (tab) => (tab.textContent || '').trim() === toLabel
          );
          from?.click();
          const selected = await waitFor(() =>
            from?.getAttribute('aria-selected') === 'true' && from
          );
          selected?.focus();
          selected?.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
          return Boolean(await waitFor(() =>
            to?.getAttribute('aria-selected') === 'true' &&
            document.activeElement === to && to
          ));
        };
        let keyboardContract = maximized;
        keyboardContract &&= await keyboardStep('Connection', 'ArrowRight', 'Importers');
        keyboardContract &&= await keyboardStep('Importers', 'End', 'Camera');
        keyboardContract &&= await keyboardStep('Camera', 'Home', 'Connection');
        keyboardContract &&= await keyboardStep('Connection', 'ArrowLeft', 'Camera');

        const legacyPanelsAbsent = !document.querySelector(
          'section.panel-layout, section.panel-style, section.panel-camera'
        );

        // Leave the familiar Connection summary selected, restore all panels,
        // and wait for the fresh Graph canvas rather than retaining a stale
        // element reference across Panel Kit's maximize/unmaximize remount.
        const liveSettings = document.querySelector('section.panel-settings');
        const connection = [...(liveSettings?.querySelectorAll('[role="tab"]') || [])]
          .find((tab) => (tab.textContent || '').trim() === 'Connection');
        connection?.click();
        const restore = liveSettings?.querySelector(
          ':scope > header.panel-head .light.max'
        );
        restore?.click();
        const workspaceRestored = Boolean(await waitFor(() =>
          !document.querySelector('.ws.maxed')
        ));
        const graphRestored = Boolean(await waitFor(() => {
          const canvas = document.querySelector('section.panel-graph canvas.graph-canvas');
          return canvas?.dataset.renderReady === 'true' &&
            Number(canvas?.dataset.nodeCount || 0) > 0;
        }));

        if (!initialPanel || !maximize || !maximized) failures.push('Settings panel did not maximize');
        if (!restore || !workspaceRestored) failures.push('Settings panel did not restore');
        if (!ariaContract) failures.push('Settings tab labels or ARIA relationships are invalid');
        if (!keyboardContract) failures.push('Settings tab keyboard navigation is invalid');
        if (!controlsHitTest) failures.push('Settings tabs are obscured from pointer input');
        if (contentPanels.length !== expected.length) failures.push('a Settings tab has no delegated content');
        if (!importerCatalog) failures.push('deployment-managed importer catalog is incomplete');
        if (!importerReadOnly) failures.push('Lavender OKF read-only PVC contract is incomplete');
        if (!importerSwitchPosture) failures.push('Importer catalog switch controls do not match the viewer posture');
        if (!legacyPanelsAbsent) failures.push('legacy Layout, Style, or Camera panel still exists');
        if (!graphRestored) failures.push('Graph renderer did not remount after Settings restore');
        return {
          ok: failures.length === 0,
          labels,
          content_panels: contentPanels,
          aria_contract: Boolean(ariaContract),
          keyboard_contract: Boolean(keyboardContract),
          controls_hit_test: Boolean(controlsHitTest),
          importer_catalog: Boolean(importerCatalog),
          importer_read_only: Boolean(importerReadOnly),
          importer_switch_posture: Boolean(importerSwitchPosture),
          legacy_panels_absent: legacyPanelsAbsent,
          graph_restored: graphRestored,
          reason: failures.length ? failures.join('; ') : null,
        };
    })()"#;
    let settings_tabs_value: serde_json::Value =
        page.evaluate(settings_tabs_js).await?.into_value()?;
    let mut settings_tabs: SettingsTabsCheck = serde_json::from_value(settings_tabs_value)
        .context("decode unified Settings regression result")?;

    // Capture visual evidence of the new app-owned importer catalog. The
    // contract check above restores the workspace first; maximize Settings a
    // second time, leave Importers selected for the screenshot, then restore
    // and prove the graph canvas remounts before continuing.
    let settings_shot_ready: bool = page
        .evaluate(
            r#"(async () => {
                const waitFor = async (predicate, timeoutMs = 10000) => {
                  const deadline = performance.now() + timeoutMs;
                  while (performance.now() < deadline) {
                    const value = predicate();
                    if (value) return value;
                    await new Promise((resolve) => setTimeout(resolve, 50));
                  }
                  return null;
                };
                const initial = document.querySelector('section.panel-settings');
                initial?.querySelector(':scope > header.panel-head .light.max')?.click();
                const panel = await waitFor(() => {
                  const candidate = document.querySelector('section.panel-settings');
                  const rect = candidate?.getBoundingClientRect();
                  return document.querySelector('.ws.maxed') &&
                    rect?.width > 900 && rect?.height > 400 && candidate;
                });
                const importer = [...(panel?.querySelectorAll('[role="tab"]') || [])]
                  .find((tab) => (tab.textContent || '').trim() === 'Importers');
                importer?.click();
                return Boolean(await waitFor(() =>
                  importer?.getAttribute('aria-selected') === 'true' &&
                  panel?.querySelector(
                    '.importer-card[data-source-id="lavender-ingest-okf"]'
                  )
                ));
            })()"#,
        )
        .await?
        .into_value()?;
    if !settings_shot_ready {
        settings_tabs.ok = false;
        settings_tabs.reason = Some(match settings_tabs.reason.take() {
            Some(reason) => format!("{reason}; Importers screenshot did not become ready"),
            None => "Importers screenshot did not become ready".to_string(),
        });
    } else {
        let settings_png = page
            .screenshot(CaptureScreenshotParams::builder().build())
            .await
            .context("Settings Importers screenshot")?;
        let settings_bytes = if settings_png.first() == Some(&0x89) {
            settings_png
        } else {
            base64::engine::general_purpose::STANDARD
                .decode(&settings_png)
                .unwrap_or(settings_png)
        };
        let settings_shot_path = args.out_dir.join("settings-importers.png");
        tokio::fs::write(&settings_shot_path, settings_bytes).await?;
        tracing::info!("wrote screenshot {}", settings_shot_path.display());
    }

    let settings_shot_restored: bool = page
        .evaluate(
            r#"(async () => {
                const waitFor = async (predicate, timeoutMs = 10000) => {
                  const deadline = performance.now() + timeoutMs;
                  while (performance.now() < deadline) {
                    const value = predicate();
                    if (value) return value;
                    await new Promise((resolve) => setTimeout(resolve, 50));
                  }
                  return null;
                };
                const settings = document.querySelector('section.panel-settings');
                const connection = [...(settings?.querySelectorAll('[role="tab"]') || [])]
                  .find((tab) => (tab.textContent || '').trim() === 'Connection');
                connection?.click();
                settings?.querySelector(':scope > header.panel-head .light.max')?.click();
                const workspace = await waitFor(() => !document.querySelector('.ws.maxed'));
                const graph = await waitFor(() => {
                  const canvas = document.querySelector('section.panel-graph canvas.graph-canvas');
                  return canvas?.dataset.renderReady === 'true' &&
                    Number(canvas?.dataset.nodeCount || 0) > 0;
                });
                return Boolean(workspace && graph);
            })()"#,
        )
        .await?
        .into_value()?;
    if !settings_shot_restored {
        settings_tabs.ok = false;
        settings_tabs.graph_restored = false;
        settings_tabs.reason = Some(match settings_tabs.reason.take() {
            Some(reason) => format!("{reason}; Graph did not restore after Importers screenshot"),
            None => "Graph did not restore after Importers screenshot".to_string(),
        });
    }

    // ---- 7. Filter is a repeatable, validated Boolean builder -------------
    // Restore the minimized Filter panel, maximize it for deterministic
    // geometry, and drive only its stable test/ARIA contract. The nested
    // group's tag pair and expected ALL/ANY counts derive from the served
    // corpus itself: the JS decodes /graph/meta_summary (the same facet
    // payload the panel evaluates field rules against) and picks two tag
    // values whose node sets have a strict intersection-is-smaller-than-union
    // relationship. The contract therefore holds against the seeded fixture
    // vault and the live github-imported corpus alike — the fixture notes
    // cannot be seeded into a deployment whose importer grants no
    // content-write effect.
    let filter_builder_js = r#"(async () => {
        const waitFor = async (predicate, timeoutMs = 10000) => {
          const deadline = performance.now() + timeoutMs;
          while (performance.now() < deadline) {
            const value = predicate();
            if (value) return value;
            await new Promise((resolve) => setTimeout(resolve, 50));
          }
          return null;
        };
        const failures = [];
        const selector = (testId) => `[data-testid="${testId}"]`;
        const own = (group, testId) => [...(group?.querySelectorAll(selector(testId)) || [])]
          .find((element) => element.closest(selector('filter-group')) === group);
        const ownRules = (group, kind) => [...(group?.querySelectorAll(
          `${selector('filter-rule')}[data-rule-kind="${kind}"]`
        ) || [])].filter((rule) => rule.closest(selector('filter-group')) === group);
        const setValue = async (control, value) => {
          if (!control) return false;
          if (control instanceof HTMLInputElement || control instanceof HTMLTextAreaElement) {
            const proto = control instanceof HTMLTextAreaElement
              ? HTMLTextAreaElement.prototype
              : HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
            setter?.call(control, value);
          } else {
            control.value = value;
          }
          control.dispatchEvent(new Event('input', { bubbles: true }));
          control.dispatchEvent(new Event('change', { bubbles: true }));
          await new Promise((resolve) => setTimeout(resolve, 75));
          return true;
        };
        const countOf = (group) => {
          const count = own(group, 'filter-match-count');
          const raw = count?.getAttribute('data-count') || count?.textContent || '';
          const match = raw.match(/\d+/);
          return match ? Number(match[0]) : 0;
        };
        const groupById = (id) => document.querySelector(
          `${selector('filter-group')}[data-group-id="${id}"]`
        );
        const ruleById = (id) => document.querySelector(
          `${selector('filter-rule')}[data-rule-id="${id}"]`
        );
        const setFieldRule = async (id, field, value) => {
          let rule = ruleById(id);
          await setValue(rule?.querySelector(selector('filter-field-name')), field);
          rule = ruleById(id);
          await setValue(rule?.querySelector(selector('filter-field-value')), value);
          return Boolean(ruleById(id));
        };
        const setMatchesOperator = async (id) => {
          for (let attempt = 0; attempt < 6; attempt += 1) {
            const rule = ruleById(id);
            const control = rule?.querySelector(selector('filter-field-operator'));
            // A <select>'s textContent concatenates EVERY option label, so it
            // always contains "matches regex" and would make this probe report
            // success before the operator was ever changed. Read textContent
            // only for a non-select control, where it reflects current state.
            const state = [
              control?.getAttribute('data-operator'),
              control?.value,
              control instanceof HTMLSelectElement ? null : control?.textContent,
              rule?.getAttribute('data-expression'),
            ].filter(Boolean).join(' ');
            if (/matches/i.test(state)) return true;
            if (control instanceof HTMLSelectElement) {
              const option = [...control.options].find((candidate) =>
                /matches/i.test(`${candidate.value} ${candidate.textContent || ''}`)
              );
              if (!option) return false;
              await setValue(control, option.value);
            } else {
              control?.click();
              await new Promise((resolve) => setTimeout(resolve, 75));
            }
          }
          return false;
        };

        // Decode /graph/meta_summary — the same facet payload the panel
        // evaluates field rules against — and pick two tag values whose node
        // sets have a strict intersection < union relationship, so the
        // nested group's ALL/ANY counts are derived from the served corpus
        // rather than from fixture notes a read-only deployment cannot hold.
        // Wire shape (prost): MetaSummary { repeated string fields = 1;
        // repeated FieldBucket buckets = 2 }; FieldBucket { uint32 field_idx
        // = 1; string value = 2; packed repeated uint32 node_idx = 3 }.
        const corpusTags = await (async () => {
          try {
            const response = await fetch('/graph/meta_summary');
            if (!response.ok) return null;
            const bytes = new Uint8Array(await response.arrayBuffer());
            const reader = (buffer) => {
              let offset = 0;
              return {
                varint() {
                  let result = 0;
                  let shift = 0;
                  for (;;) {
                    if (offset >= buffer.length) throw new Error('truncated varint');
                    const byte = buffer[offset];
                    offset += 1;
                    result += (byte & 0x7f) * 2 ** shift;
                    if (!(byte & 0x80)) return result;
                    shift += 7;
                  }
                },
                take(length) {
                  if (offset + length > buffer.length) throw new Error('truncated field');
                  const slice = buffer.slice(offset, offset + length);
                  offset += length;
                  return slice;
                },
                get done() { return offset >= buffer.length; },
              };
            };
            const top = reader(bytes);
            const fields = [];
            const buckets = [];
            while (!top.done) {
              const key = top.varint();
              if (key % 8 !== 2) throw new Error('unexpected wire type');
              const field = Math.floor(key / 8);
              const payload = top.take(top.varint());
              if (field === 1) fields.push(new TextDecoder().decode(payload));
              if (field === 2) buckets.push(payload);
            }
            const tags = new Map();
            for (const bucket of buckets) {
              const inner = reader(bucket);
              let fieldIdx = null;
              let value = null;
              let nodes = [];
              while (!inner.done) {
                const key = inner.varint();
                const field = Math.floor(key / 8);
                if (key % 8 === 0) {
                  const scalar = inner.varint();
                  if (field === 1) fieldIdx = scalar;
                } else if (key % 8 === 2) {
                  const payload = inner.take(inner.varint());
                  if (field === 2) value = new TextDecoder().decode(payload);
                  if (field === 3) {
                    const packed = reader(payload);
                    nodes = [];
                    while (!packed.done) nodes.push(packed.varint());
                  }
                } else {
                  throw new Error('unexpected wire type');
                }
              }
              if (fieldIdx !== null && fields[fieldIdx] === 'tags' && value) {
                tags.set(value, nodes);
              }
            }
            return tags;
          } catch {
            return null;
          }
        })();
        // Regex-safe values only: the pair later feeds the Matches operator.
        const tagCandidates = [...(corpusTags?.entries() || [])]
          .filter(([value]) => /^[A-Za-z0-9/_-]+$/.test(value))
          .sort((a, b) => a[1].length - b[1].length ||
            (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0))
          .slice(0, 200);
        let tagPair = null;
        let disjointPair = null;
        for (let i = 0; i < tagCandidates.length && !tagPair; i += 1) {
          for (let j = i + 1; j < tagCandidates.length; j += 1) {
            const first = new Set(tagCandidates[i][1]);
            const second = new Set(tagCandidates[j][1]);
            const intersection = [...first].filter((node) => second.has(node)).length;
            const union = new Set([...first, ...second]).size;
            if (union <= intersection) continue;
            const pair = {
              a: tagCandidates[i][0],
              b: tagCandidates[j][0],
              all: intersection,
              any: union,
            };
            if (intersection >= 1) {
              tagPair = pair;
              break;
            }
            disjointPair = disjointPair || pair;
          }
        }
        tagPair = tagPair || disjointPair;

        const dockChip = [...document.querySelectorAll('.dock-chip')]
          .find((chip) => (chip.textContent || '').trim() === 'Filter');
        dockChip?.click();
        const initialPanel = await waitFor(() => document.querySelector('section.panel-filter'));
        const maximize = initialPanel?.querySelector(
          ':scope > header.panel-head .light.max'
        );
        maximize?.click();
        const panel = await waitFor(() => {
          const candidate = document.querySelector('section.panel-filter');
          const rect = candidate?.getBoundingClientRect();
          return document.querySelector('.ws.maxed') && rect?.width > 900 &&
            rect?.height > 400 && candidate;
        });
        const builder = await waitFor(() => panel?.querySelector(selector('filter-builder')));
        const panelVisible = Boolean(panel && builder);
        builder?.querySelector(selector('filter-reset'))?.click();

        let root = await waitFor(() => {
          const candidate = builder?.querySelector(selector('filter-group'));
          return candidate?.getAttribute('data-group-id') && candidate;
        });
        const rootId = root?.getAttribute('data-group-id') || '';

        own(root, 'filter-add-search')?.click();
        await waitFor(() => ownRules(groupById(rootId), 'search').length === 1);
        root = groupById(rootId);
        own(root, 'filter-add-search')?.click();
        const searchesAdded = await waitFor(() => {
          const rules = ownRules(groupById(rootId), 'search');
          return rules.length === 2 && rules;
        });
        let searchRules = searchesAdded || [];
        const firstSearchId = searchRules[0]?.getAttribute('data-rule-id') || '';
        const secondSearchId = searchRules[1]?.getAttribute('data-rule-id') || '';
        await setValue(
          ruleById(firstSearchId)?.querySelector(selector('filter-search-query')),
          'BROWSER_NODE_EDITOR_SENTINEL'
        );
        await setValue(
          ruleById(secondSearchId)?.querySelector(selector('filter-search-query')),
          'Shared tag sibling'
        );

        root = groupById(rootId);
        searchRules = ownRules(root, 'search');
        const secondBeforeMove = searchRules.find(
          (rule) => rule.getAttribute('data-rule-id') === secondSearchId
        );
        const moveUp = [...(secondBeforeMove?.querySelectorAll('button[aria-label]') || [])]
          .find((button) => /^Move rule .+ up$/.test(button.getAttribute('aria-label') || ''));
        moveUp?.click();
        const reordered = await waitFor(() => {
          const liveRoot = groupById(rootId);
          const ids = ownRules(liveRoot, 'search')
            .map((rule) => rule.getAttribute('data-rule-id'));
          return ids[0] === secondSearchId && ids[1] === firstSearchId && ids;
        });
        const accessibleReorder = Boolean(
          moveUp && /^Move rule .+ up$/.test(moveUp.getAttribute('aria-label') || '') && reordered
        );
        searchRules = ownRules(groupById(rootId), 'search');
        const searchValues = searchRules.map((rule) =>
          rule.querySelector(selector('filter-search-query'))?.value || ''
        );
        const independentSearchRules = searchRules.length === 2 &&
          new Set(searchRules.map((rule) => rule.getAttribute('data-rule-id'))).size === 2 &&
          searchValues.includes('BROWSER_NODE_EDITOR_SENTINEL') &&
          searchValues.includes('Shared tag sibling') &&
          searchRules.every((rule) => Boolean(rule.getAttribute('data-expression')));

        root = groupById(rootId);
        own(root, 'filter-add-group')?.click();
        const nested = await waitFor(() => [...(builder?.querySelectorAll(
          selector('filter-group')
        ) || [])].find((group) => group.getAttribute('data-group-id') !== rootId));
        const nestedId = nested?.getAttribute('data-group-id') || '';
        const firstAddField = await waitFor(() => {
          const button = own(groupById(nestedId), 'filter-add-field');
          return button && !button.disabled && button;
        });
        firstAddField?.click();
        await waitFor(() => ownRules(groupById(nestedId), 'field').length === 1);
        own(groupById(nestedId), 'filter-add-field')?.click();
        const fieldsAdded = await waitFor(() => {
          const rules = ownRules(groupById(nestedId), 'field');
          return rules.length === 2 && rules;
        });
        let fieldRules = fieldsAdded || [];
        const editorFieldId = fieldRules[0]?.getAttribute('data-rule-id') || '';
        const sharedFieldId = fieldRules[1]?.getAttribute('data-rule-id') || '';
        let allCount = 0;
        let anyCount = 0;
        let modeCounts = false;
        let fieldsPresent = false;
        let inlineDiagnostic = false;
        let lastValidState = false;
        if (tagPair) {
          await setFieldRule(editorFieldId, 'tags', tagPair.a);
          await setFieldRule(sharedFieldId, 'tags', tagPair.b);

          const allReady = await waitFor(() => {
            const group = groupById(nestedId);
            return group?.getAttribute('data-mode') === 'all' &&
              countOf(group) === tagPair.all && group;
          });
          allCount = allReady ? countOf(allReady) : 0;
          own(groupById(nestedId), 'filter-group-mode')?.parentElement
            ?.querySelector('[data-testid="filter-group-mode"][data-mode-target="any"]')
            ?.click();
          const anyReady = await waitFor(() => {
            const group = groupById(nestedId);
            return group?.getAttribute('data-mode') === 'any' &&
              countOf(group) === tagPair.any && group;
          });
          anyCount = anyReady ? countOf(anyReady) : 0;
          modeCounts = tagPair.any > tagPair.all &&
            allCount === tagPair.all && anyCount === tagPair.any;
          fieldRules = ownRules(groupById(nestedId), 'field');
          const fieldValues = fieldRules.map((rule) =>
            rule.querySelector(selector('filter-field-value'))?.value || ''
          );
          fieldsPresent = fieldRules.length === 2 &&
            fieldValues.includes(tagPair.a) && fieldValues.includes(tagPair.b) &&
            fieldRules.every((rule) =>
              (rule.querySelector(selector('filter-field-name'))?.value || '') === 'tags' &&
              Boolean(rule.getAttribute('data-expression'))
            );

          const matchesOperator = await setMatchesOperator(editorFieldId);
          // Matches is unanchored; anchor the pattern so it selects exactly
          // the same bucket the exact-match operator did.
          await setValue(
            ruleById(editorFieldId)?.querySelector(selector('filter-field-value')),
            `^${tagPair.a}$`
          );
          const appliedBeforeInvalid = await waitFor(() => {
            const evaluation = builder?.querySelector(selector('filter-evaluation'));
            return evaluation?.getAttribute('data-phase') === 'applied' &&
              evaluation?.getAttribute('data-applied-count') !== null && evaluation;
          });
          const lastAppliedCount = appliedBeforeInvalid?.getAttribute('data-applied-count');
          await setValue(
            ruleById(editorFieldId)?.querySelector(selector('filter-field-value')),
            '['
          );
          const diagnostic = await waitFor(() => {
            const rule = ruleById(editorFieldId);
            const alert = rule?.querySelector(`${selector('filter-diagnostic')}[role="alert"]`);
            return /Invalid regular expression/i.test(alert?.textContent || '') && alert;
          });
          const invalidEvaluation = builder?.querySelector(selector('filter-evaluation'));
          inlineDiagnostic = Boolean(matchesOperator && diagnostic);
          lastValidState = Boolean(
            appliedBeforeInvalid && invalidEvaluation?.getAttribute('data-phase') === 'invalid' &&
            invalidEvaluation?.getAttribute('data-applied-count') === lastAppliedCount &&
            countOf(groupById(nestedId)) === anyCount
          );
        }

        if (!dockChip || !initialPanel || !maximize || !panelVisible) {
          failures.push('Filter did not restore from the dock and maximize');
        }
        if (!independentSearchRules) failures.push('repeatable Search rules are not independent');
        if (!accessibleReorder) failures.push('Search rules did not expose a working accessible reorder');
        if (!tagPair) failures.push('served corpus has no tag pair with a strict ALL/ANY relationship');
        if (!fieldsPresent) failures.push('tag field rules are incomplete');
        if (!modeCounts) failures.push(
          `nested ALL/ANY counts were not ${tagPair?.all ?? '?'} and ${tagPair?.any ?? '?'}`
        );
        if (!inlineDiagnostic) failures.push('invalid regex has no inline diagnostic');
        if (!lastValidState) failures.push('invalid draft did not preserve the last valid result');
        return {
          ok: failures.length === 0,
          panel_visible: panelVisible,
          independent_search_rules: independentSearchRules,
          field_rules: fieldsPresent,
          all_count: allCount,
          any_count: anyCount,
          mode_counts: modeCounts,
          inline_diagnostic: inlineDiagnostic,
          last_valid_state: lastValidState,
          accessible_reorder: accessibleReorder,
          graph_restored: false,
          reason: failures.length ? failures.join('; ') : null,
        };
    })()"#;
    let filter_builder_value: serde_json::Value =
        page.evaluate(filter_builder_js).await?.into_value()?;
    let mut filter_builder: FilterBuilderCheck = serde_json::from_value(filter_builder_value)
        .context("decode Filter builder regression result")?;

    let filter_png = page
        .screenshot(CaptureScreenshotParams::builder().build())
        .await
        .context("Filter builder screenshot")?;
    let filter_bytes = if filter_png.first() == Some(&0x89) {
        filter_png
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(&filter_png)
            .unwrap_or(filter_png)
    };
    let filter_shot_path = args.out_dir.join("filter-builder.png");
    tokio::fs::write(&filter_shot_path, filter_bytes).await?;
    tracing::info!("wrote screenshot {}", filter_shot_path.display());

    // Reset the deliberately invalid draft before restoring the full
    // workspace. This leaves subsequent Graph checks independent of the
    // Filter regression while proving the renderer remount path again.
    let filter_shot_restored: bool = page
        .evaluate(
            r#"(async () => {
                const waitFor = async (predicate, timeoutMs = 10000) => {
                  const deadline = performance.now() + timeoutMs;
                  while (performance.now() < deadline) {
                    const value = predicate();
                    if (value) return value;
                    await new Promise((resolve) => setTimeout(resolve, 50));
                  }
                  return null;
                };
                const panel = document.querySelector('section.panel-filter');
                panel?.querySelector('[data-testid="filter-reset"]')?.click();
                await waitFor(() => {
                  const builder = panel?.querySelector('[data-testid="filter-builder"]');
                  return !builder?.querySelector('[data-testid="filter-rule"]') && builder;
                });
                if (document.querySelector('.ws.maxed')) {
                  panel?.querySelector(':scope > header.panel-head .light.max')?.click();
                }
                const workspace = await waitFor(() => !document.querySelector('.ws.maxed'));
                const graph = await waitFor(() => {
                  const canvas = document.querySelector('section.panel-graph canvas.graph-canvas');
                  return canvas?.dataset.renderReady === 'true' &&
                    Number(canvas?.dataset.nodeCount || 0) > 0;
                });
                return Boolean(workspace && graph);
            })()"#,
        )
        .await?
        .into_value()?;
    filter_builder.graph_restored = filter_shot_restored;
    if !filter_shot_restored {
        filter_builder.ok = false;
        filter_builder.reason = Some(match filter_builder.reason.take() {
            Some(reason) => format!("{reason}; Graph did not restore after Filter screenshot"),
            None => "Graph did not restore after Filter screenshot".to_string(),
        });
    }

    // ---- 8. Graph header actions are present, visible, and safe ----------
    // Exercise the controls through the same pointer/mouse event sequence a
    // user generates. Holding pointerdown across two animation frames catches
    // accidental propagation into Panel Kit's panel-drag handlers.
    let header_actions_js = r#"(async () => {
        const panel = document.querySelector('section.panel-graph');
        const header = panel?.querySelector(':scope > header.panel-head');
        const root = panel?.closest('.ws-root') || document.querySelector('.ws-root');
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
        const canvas = panel?.querySelector('canvas.graph-canvas');
        const renderReady = canvas?.dataset.renderReady === 'true';
        const secureContext = window.isSecureContext === true;
        const webgpuAvailable = Boolean(navigator.gpu);
        const nodeCount = Number(canvas?.dataset.nodeCount || 0);
        const clicksOk = !dragStarted && canvasPresent;
        const failures = [];
        if (!panel || !header) failures.push('Graph panel header missing');
        if (!playPauseFound) failures.push('play/pause action missing');
        if (!fitFound) failures.push('Fit action missing');
        if (!geometryOk) failures.push('header action clipped or zero-sized');
        if (dragStarted) failures.push('header action started a panel drag');
        if (!canvasPresent) failures.push('Graph canvas missing after action click');
        if (!secureContext) failures.push('browser page is not a secure context');
        if (!webgpuAvailable) failures.push('navigator.gpu is unavailable');
        if (!renderReady) failures.push('WebGPU render host did not initialize');
        if (nodeCount < 1) failures.push('frontend did not load any graph nodes');
        return {
          ok: failures.length === 0,
          action_count: buttons.length,
          play_pause_found: playPauseFound,
          fit_found: fitFound,
          geometry_ok: geometryOk,
          clicks_ok: clicksOk,
          drag_started: dragStarted,
          canvas_present: canvasPresent,
          render_ready: renderReady,
          secure_context: secureContext,
          webgpu_available: webgpuAvailable,
          node_count: nodeCount,
          reason: failures.length ? failures.join('; ') : null,
          actions: details,
        };
    })()"#;
    let header_actions_value: serde_json::Value =
        page.evaluate(header_actions_js).await?.into_value()?;
    let header_actions: HeaderActionCheck = serde_json::from_value(header_actions_value)
        .context("decode Graph header action regression result")?;

    // ---- 9. canvas exists with non-zero size -----------------------------
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

    // ---- 10. screenshot --------------------------------------------------
    let shot_params = CaptureScreenshotParams::builder().build();
    let png_b64 = page.screenshot(shot_params).await.context("screenshot")?;
    let bytes: Vec<u8> = png_b64;
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

    // ---- 11. Sessions view mounts the world workspace -------------------
    // View switches mount IN PLACE (persisted `jc_view` + distinct
    // UserWorkspaceView/SessionsWorkspaceView component types): the flush
    // unmounts one hook scope and initializes the other without a page
    // reload, so assertions run in fresh evaluates against the live page.
    // With no session-manager URL configured the app boots the embedded
    // single-user host, so the contract is deterministic: Worlds panel with
    // its empty state, dock present, and switching back to User remounts the
    // graph canvas.
    let click_sessions_js = r#"(() => {
        const buttons = [...(document.querySelector('nav.view-switch')
          ?.querySelectorAll('button.view-btn') || [])];
        const userButton = buttons.find(
          (button) => (button.textContent || '').trim() === 'User'
        );
        const sessionsButton = buttons.find(
          (button) => (button.textContent || '').trim() === 'Sessions'
        );
        const switchFound = Boolean(userButton && sessionsButton);
        sessionsButton?.click();
        return { switch_found: switchFound };
    })()"#;
    let click_value: serde_json::Value = page.evaluate(click_sessions_js).await?.into_value()?;
    let switch_found = click_value
        .get("switch_found")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Poll (in fresh evaluates) until the Sessions button reports active.
    let sessions_state_js = r#"(() => {
        const buttons = [...(document.querySelector('nav.view-switch')
          ?.querySelectorAll('button.view-btn') || [])];
        const sessions = buttons.find(
          (button) => (button.textContent || '').trim() === 'Sessions'
        );
        return {
          active: Boolean(sessions?.classList.contains('active')),
        };
    })()"#;
    let mut sessions_active = false;
    for _ in 0..50 {
        let eval = page.evaluate(sessions_state_js).await?;
        let v: serde_json::Value = eval.into_value()?;
        if v.get("active").and_then(|a| a.as_bool()).unwrap_or(false) {
            sessions_active = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let sessions_panels_js = r#"(async () => {
        const waitFor = async (predicate, timeoutMs = 10000) => {
          const deadline = performance.now() + timeoutMs;
          while (performance.now() < deadline) {
            const value = predicate();
            if (value) return value;
            await new Promise((resolve) => setTimeout(resolve, 50));
          }
          return null;
        };
        const worldsPanel = await waitFor(() => {
          const panel = document.querySelector('section.panel-worlds');
          const title = panel?.querySelector(':scope > header.panel-head .panel-title');
          return (title?.textContent || '').trim() === 'Worlds' && panel;
        });
        const worldsEmptyState = Boolean(await waitFor(() => {
          const empty = worldsPanel?.querySelector('.empty');
          return /no worlds yet/i.test(empty?.textContent || '') && empty;
        }));
        const dockPresent = Boolean(await waitFor(() =>
          document.querySelector('footer.dock')
        ));
        return {
          worlds_panel: Boolean(worldsPanel),
          worlds_empty_state: worldsEmptyState,
          dock_present: dockPresent,
        };
    })()"#;
    let panels_value: serde_json::Value =
        page.evaluate(sessions_panels_js).await?.into_value()?;
    let worlds_visible = panels_value
        .get("worlds_panel")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let worlds_empty_state = panels_value
        .get("worlds_empty_state")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dock_present = panels_value
        .get("dock_present")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut failures: Vec<String> = Vec::new();
    if !switch_found {
        failures.push("topbar view switcher missing User/Sessions buttons".to_string());
    }
    if !sessions_active {
        failures.push("Sessions button did not become active".to_string());
    }
    if !worlds_visible {
        failures.push("Worlds panel missing in the Sessions workspace".to_string());
    }
    if !worlds_empty_state {
        failures.push("embedded-host Worlds empty state missing".to_string());
    }
    if !dock_present {
        failures.push("Sessions workspace dock missing".to_string());
    }
    let mut sessions_view = SessionsViewCheck {
        ok: failures.is_empty(),
        switch_found,
        sessions_active,
        worlds_panel: worlds_visible,
        worlds_empty_state,
        dock_present,
        user_restored: false,
        reason: if failures.is_empty() {
            None
        } else {
            Some(failures.join("; "))
        },
    };

    let sessions_png = page
        .screenshot(CaptureScreenshotParams::builder().build())
        .await
        .context("Sessions view screenshot")?;
    let sessions_bytes = if sessions_png.first() == Some(&0x89) {
        sessions_png
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(&sessions_png)
            .unwrap_or(sessions_png)
    };
    let sessions_shot_path = args.out_dir.join("sessions-view.png");
    tokio::fs::write(&sessions_shot_path, sessions_bytes).await?;
    tracing::info!("wrote screenshot {}", sessions_shot_path.display());

    // Leave the app back on the User view: proves the switch is reversible
    // and keeps any later assertions on the default surface.
    let click_user_js = r#"(() => {
        const buttons = [...(document.querySelector('nav.view-switch')
          ?.querySelectorAll('button.view-btn') || [])];
        const user = buttons.find(
          (button) => (button.textContent || '').trim() === 'User'
        );
        user?.click();
        return true;
    })()"#;
    page.evaluate(click_user_js).await?;
    let user_state_js = r#"(() => {
        const buttons = [...(document.querySelector('nav.view-switch')
          ?.querySelectorAll('button.view-btn') || [])];
        const user = buttons.find(
          (button) => (button.textContent || '').trim() === 'User'
        );
        const canvas = document.querySelector('section.panel-graph canvas.graph-canvas');
        return {
          active: Boolean(user?.classList.contains('active')),
          graph_ready: canvas?.dataset.renderReady === 'true' &&
            Number(canvas?.dataset.nodeCount || 0) > 0,
        };
    })()"#;
    let mut user_restored = false;
    for _ in 0..50 {
        let eval = page.evaluate(user_state_js).await?;
        let v: serde_json::Value = eval.into_value()?;
        let active = v.get("active").and_then(|a| a.as_bool()).unwrap_or(false);
        let graph_ready = v
            .get("graph_ready")
            .and_then(|g| g.as_bool())
            .unwrap_or(false);
        if active && graph_ready {
            user_restored = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    sessions_view.user_restored = user_restored;
    if !user_restored {
        sessions_view.ok = false;
        sessions_view.reason = Some(match sessions_view.reason.take() {
            Some(reason) => format!("{reason}; User view did not restore a render-ready graph"),
            None => "User view did not restore a render-ready graph".to_string(),
        });
    }

    // Open-world persistence: the world open in the Sessions view survives a
    // full-page reload via the `jc_world` localStorage key (boot restore in
    // the host-rebuild effect). Create + open a world on the embedded host,
    // reload, and require the topbar to show the world again; then close it
    // so the selection (and key) clear. Driven step by step so each stage
    // can time out on its own.
    let world_flow = async |page: &chromiumoxide::Page| -> Result<Vec<&'static str>> {
        let mut failures: Vec<&'static str> = Vec::new();
        // Sessions view (may already be active — the click then no-ops).
        page.evaluate(
            r#"(() => {
                const buttons = [...(document.querySelector('nav.view-switch')
                  ?.querySelectorAll('button.view-btn') || [])];
                buttons.find((b) => (b.textContent || '').trim() === 'Sessions')?.click();
                return true;
            })()"#,
        )
        .await?;
        // Fill the world-name input (native setter + input event so Dioxus
        // sees it), then wait for the Create button to enable before
        // clicking — Dioxus applies the input event a tick after dispatch.
        let fill_js = r#"(() => {
            const input = document.querySelector(
              'section.panel-worlds input[aria-label="World name"]');
            if (!input) return false;
            const setter = Object.getOwnPropertyDescriptor(
              window.HTMLInputElement.prototype, 'value').set;
            setter.call(input, 'browser-e2e');
            input.dispatchEvent(new Event('input', { bubbles: true }));
            return true;
        })()"#;
        let create_js = r#"(() => {
            const create = [...document.querySelectorAll(
              'section.panel-worlds .controls button')]
              .find((b) => (b.textContent || '').trim() === 'Create');
            if (!create) return 'no-create';
            if (create.disabled) return 'disabled';
            create.click();
            return 'ok';
        })()"#;
        let mut created = false;
        for _ in 0..50 {
            let filled: serde_json::Value = page.evaluate(fill_js).await?.into_value()?;
            if !filled.as_bool().unwrap_or(false) {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
            let v: serde_json::Value = page.evaluate(create_js).await?.into_value()?;
            match v.as_str().unwrap_or("") {
                "ok" => {
                    created = true;
                    break;
                }
                "no-create" => break, // input present but button missing: dead end
                _ => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !created {
            failures.push("world create form never became operable");
            return Ok(failures);
        }
        // Wait for the world row, then click its Open button.
        let open_js = r#"(() => {
            const row = [...document.querySelectorAll('section.panel-worlds .kv')]
              .find((r) => (r.textContent || '').includes('browser-e2e'));
            if (!row) return 'no-row';
            const open = [...row.querySelectorAll('button')]
              .find((b) => (b.textContent || '').trim() === 'Open');
            if (!open) return 'no-open: ' + (row.innerHTML || '').slice(0, 300);
            open.click();
            return 'ok';
        })()"#;
        let mut opened = false;
        let mut last_no_open = String::new();
        for _ in 0..50 {
            let v: serde_json::Value = page.evaluate(open_js).await?.into_value()?;
            let s = v.as_str().unwrap_or("");
            if s == "ok" {
                opened = true;
                break;
            }
            if let Some(detail) = s.strip_prefix("no-open: ") {
                last_no_open = detail.to_string();
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !opened {
            // `last_no_open` carries the row markup when the row existed but
            // the Open button was never found.
            eprintln!("world row never offered Open; last row markup: {last_no_open}");
            failures.push("world row never offered Open");
            return Ok(failures);
        }
        // Wait until the topbar reports the open world.
        let label_js = r#"(() => {
            const hint = document.querySelector('header.topbar .hint');
            return {
              open: (hint?.textContent || '').includes('world browser-e2e'),
              canvas: Boolean(document.querySelector(
                'section.panel-graph canvas.graph-canvas')),
              boot: window.__jc_boot || 0,
            };
        })()"#;
        let mut label_before = false;
        let mut boot_before = 0.0_f64;
        for _ in 0..50 {
            let v: serde_json::Value = page.evaluate(label_js).await?.into_value()?;
            boot_before = v.get("boot").and_then(|b| b.as_f64()).unwrap_or(0.0);
            if v.get("open").and_then(|o| o.as_bool()).unwrap_or(false) {
                label_before = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !label_before {
            failures.push("opened world never showed in the topbar");
            return Ok(failures);
        }
        // Reload the page; the boot restore must re-open the world and
        // remount its canvas (the embedded host replays the world closed, so
        // this also proves the reopen-and-rematerialize path).
        page.evaluate("(() => { location.reload(); return true; })()")
            .await?;
        let mut restored = false;
        for _ in 0..100 {
            // Evaluates can race the navigation and lose their execution
            // context — retry rather than fail.
            let Ok(eval) = page.evaluate(label_js).await else {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                continue;
            };
            let v: serde_json::Value = eval.into_value()?;
            let boot = v.get("boot").and_then(|b| b.as_f64()).unwrap_or(0.0);
            let open = v.get("open").and_then(|o| o.as_bool()).unwrap_or(false);
            let canvas = v.get("canvas").and_then(|c| c.as_bool()).unwrap_or(false);
            if boot != boot_before && open && canvas {
                restored = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        if !restored {
            failures.push("open world did not restore across a page reload (label + canvas)");
            return Ok(failures);
        }
        // Close the world: selection + persisted key must clear.
        let close_js = r#"(() => {
            const row = [...document.querySelectorAll('section.panel-worlds .kv')]
              .find((r) => (r.textContent || '').includes('browser-e2e'));
            const close = row && [...row.querySelectorAll('button')]
              .find((b) => (b.textContent || '').trim() === 'Close');
            close?.click();
            return Boolean(close);
        })()"#;
        let mut cleared = false;
        for _ in 0..50 {
            let closed: serde_json::Value = page.evaluate(close_js).await?.into_value()?;
            let v: serde_json::Value = page.evaluate(label_js).await?.into_value()?;
            let still_open = v.get("open").and_then(|o| o.as_bool()).unwrap_or(false);
            if closed.as_bool().unwrap_or(false) && !still_open {
                cleared = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !cleared {
            failures.push("closing the world did not clear the topbar selection");
        }
        Ok(failures)
    };
    match world_flow(&page).await {
        Ok(failures) if failures.is_empty() => {}
        Ok(failures) => {
            sessions_view.ok = false;
            sessions_view.reason = Some(match sessions_view.reason.take() {
                Some(reason) => format!("{reason}; {}", failures.join("; ")),
                None => failures.join("; "),
            });
        }
        Err(e) => {
            sessions_view.ok = false;
            sessions_view.reason = Some(match sessions_view.reason.take() {
                Some(reason) => format!("{reason}; world persistence pass errored: {e:#}"),
                None => format!("world persistence pass errored: {e:#}"),
            });
        }
    }

    // Maximized-panel regression: mount the Sessions workspace while its
    // persisted layout holds a maximized panel — the flush path a prior
    // `GlobalSignal::write() → AlreadyBorrowed` investigation flagged. The
    // in-place switch must keep the app alive (evaluates keep answering)
    // with the maximized Worlds panel rendered; then restore the layout and
    // leave the app on the User view.
    let maximize_roundtrip_js = |extra: &str| {
        format!(
            r#"(() => {{
                const buttons = [...(document.querySelector('nav.view-switch')
                  ?.querySelectorAll('button.view-btn') || [])];
                const byLabel = (label) => buttons.find(
                  (button) => (button.textContent || '').trim() === label
                );
                {extra}
                return true;
            }})()"#
        )
    };
    page.evaluate(maximize_roundtrip_js("byLabel('Sessions')?.click();"))
        .await?;
    let max_light_js = r#"(() => {
        const light = document.querySelector('section.panel-worlds .light.max');
        light?.click();
        return Boolean(light);
    })()"#;
    // Wait for the Worlds panel, maximize it, switch away and back.
    let mut maximized_ok = false;
    for _ in 0..50 {
        let hit: serde_json::Value = page.evaluate(max_light_js).await?.into_value()?;
        if hit.as_bool().unwrap_or(false) {
            maximized_ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    page.evaluate(maximize_roundtrip_js("byLabel('User')?.click();"))
        .await?;
    page.evaluate(maximize_roundtrip_js("byLabel('Sessions')?.click();"))
        .await?;
    let maxed_state_js = r#"(() => {
        const panel = document.querySelector('section.panel-worlds');
        const maxed = Boolean(panel?.querySelector('.max-hint'));
        const others = [...document.querySelectorAll('section.panel')]
          .filter((p) => p !== panel).length;
        return { maxed, others };
    })()"#;
    let mut maximized_mounted = false;
    for _ in 0..50 {
        let v: serde_json::Value = page.evaluate(maxed_state_js).await?.into_value()?;
        let maxed = v.get("maxed").and_then(|m| m.as_bool()).unwrap_or(false);
        let others = v.get("others").and_then(|o| o.as_u64()).unwrap_or(0);
        if maxed && others == 0 {
            maximized_mounted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !(maximized_ok && maximized_mounted) {
        sessions_view.ok = false;
        sessions_view.reason = Some(match sessions_view.reason.take() {
            Some(reason) => format!(
                "{reason}; maximized-panel round-trip failed (maximized_ok={maximized_ok}, mounted={maximized_mounted})"
            ),
            None => format!(
                "maximized-panel round-trip failed (maximized_ok={maximized_ok}, mounted={maximized_mounted})"
            ),
        });
    }
    // Restore: un-maximize Worlds and leave the app on the User view with a
    // render-ready graph for any later assertions.
    page.evaluate(max_light_js).await?;
    page.evaluate(maximize_roundtrip_js("byLabel('User')?.click();"))
        .await?;
    for _ in 0..50 {
        let eval = page.evaluate(user_state_js).await?;
        let v: serde_json::Value = eval.into_value()?;
        let active = v.get("active").and_then(|a| a.as_bool()).unwrap_or(false);
        let graph_ready = v
            .get("graph_ready")
            .and_then(|g| g.as_bool())
            .unwrap_or(false);
        if active && graph_ready {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // ---- 12. runtime per-viewer importer switching -------------------------
    // The second fixture server was set up in parallel (3b). The authorized
    // page's console/exception streams feed the shared error gate; the
    // unauthorized page deliberately triggers a 403 flow, so its console is
    // not gated (the wire-level 403 is asserted directly over raw HTTP).
    let importer_switch = match switch_setup.await {
        Ok(Ok(Some(fixture))) => {
            run_switch_scenario(browser, fixture, console_logs.clone()).await
        }
        Ok(Ok(None)) => ImporterSwitchCheck::skipped(),
        Ok(Err(error)) => ImporterSwitchCheck::failed(format!("switch fixture setup: {error:#}")),
        Err(error) => ImporterSwitchCheck::failed(format!("switch fixture task: {error}")),
    };

    // Tear down pumps (browser close in caller will end them anyway).
    console_pump.abort();
    runtime_pump.abort();
    exception_pump.abort();
    let _ = console_pump.await;
    let _ = runtime_pump.await;
    let _ = exception_pump.await;

    Ok(RunOk {
        canvas_width,
        canvas_height,
        boot_log_found,
        pre_wasm_mount,
        header_actions,
        nodes_editor,
        settings_tabs,
        filter_builder,
        sessions_view,
        importer_switch,
    })
}

/// Decide whether the belt-and-suspenders raw TCP probe can handle this URL.
/// HTTPS deliberately goes straight to Chromium so the browser performs the
/// real certificate, secure-context, and private-network authentication path.
fn raw_http_probe_required(url: &str) -> Result<bool> {
    if url.starts_with("http://") {
        Ok(true)
    } else if url.starts_with("https://") {
        Ok(false)
    } else {
        Err(anyhow!(
            "only http:// or https:// URLs supported, got {url}"
        ))
    }
}

/// Poll a plain-HTTP base URL with a raw TCP+HTTP/1.1 handshake. We avoid
/// pulling a second HTTP client just for the local liveness probe; HTTPS is
/// navigated by Chromium instead.
async fn probe_server(url: &str, timeout: Duration) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let (host, port, path) = parse_url(url)?;
    let deadline = Instant::now() + timeout;
    loop {
        let attempt = async {
            let mut stream = TcpStream::connect((host.as_str(), port)).await?;
            let req =
                format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
            stream.write_all(req.as_bytes()).await?;
            let mut buf = Vec::with_capacity(512);
            // Read up to first chunk; we just need the status line.
            let _ =
                tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf)).await;
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

// --- runtime importer switching fixture ---------------------------------------

/// Group the second fixture server requires for runtime switching. The
/// authorized page carries it via `Network.setExtraHTTPHeaders`, simulating
/// the authenticating proxy's group injection.
const SWITCH_GROUP: &str = "test-admins";
const SWITCH_DEFAULT_ID: &str = "default-obsidian";
const SWITCH_ALT_ID: &str = "alt-obsidian";
const SWITCH_DEFAULT_NODE: &str = "Switch Default Fixture";
const SWITCH_ALT_NODE: &str = "Switch Alt Fixture";

/// The second fixture server: a two-source Obsidian catalog (default vault +
/// tiny alternate vault) with `JUMP_CANNON_IMPORTER_SWITCH_GROUP` set, serving
/// the same app dist as `--base-url` from a local mirror.
struct SwitchFixture {
    base_url: String,
    server: tokio::process::Child,
    work_dir: PathBuf,
}

/// Locate the `graph-api` binary: explicit `GRAPH_API_BIN` first, then PATH
/// (the `test-browser-rust` wrapper puts it there via `runtimeInputs`).
fn find_graph_api_bin() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("GRAPH_API_BIN") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("graph-api"))
        .find(|candidate| candidate.is_file())
}

async fn pick_free_port() -> Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    Ok(listener.local_addr()?.port())
}

/// Raw HTTP GET returning only the status code — the wire-contract half of
/// the switch scenario (403/200/404) needs no browser.
async fn raw_get_status(url: &str, headers: &[(&str, &str)]) -> Result<u16> {
    let (status, _) = raw_get(url, headers).await?;
    Ok(status)
}

/// Minimal HTTP/1.1 GET over a closing connection: enough to mirror the app
/// dist and to assert the switch gate's status codes without pulling an
/// HTTP client dependency into the harness.
async fn raw_get(url: &str, headers: &[(&str, &str)]) -> Result<(u16, Vec<u8>)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (host, port, path) = parse_url(url)?;
    let mut stream = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
    let mut request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut buf = Vec::with_capacity(512);
    tokio::time::timeout(Duration::from_secs(120), stream.read_to_end(&mut buf))
        .await
        .map_err(|_| anyhow!("raw GET {url} timed out"))??;
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("raw GET {url}: no header terminator"))?;
    let head = String::from_utf8_lossy(&buf[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|token| token.parse::<u16>().ok())
        .ok_or_else(|| {
            anyhow!(
                "no HTTP status in response: {}",
                head.lines().next().unwrap_or("<empty>")
            )
        })?;
    let body = &buf[split + 4..];
    let chunked = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    let body = if chunked {
        decode_chunked(body).with_context(|| format!("raw GET {url}: chunked body"))?
    } else {
        body.to_vec()
    };
    Ok((status, body))
}

fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let pos = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow!("missing chunk-size terminator"))?;
        let size_text = std::str::from_utf8(&body[..pos])?;
        let size = usize::from_str_radix(
            size_text.split(';').next().unwrap_or("").trim(),
            16,
        )
        .map_err(|e| anyhow!("bad chunk size {size_text:?}: {e}"))?;
        body = &body[pos + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size + 2 {
            bail!("truncated chunk");
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
    Ok(out)
}

/// Replace `__TOKEN__`-style placeholders in a JS block.
fn js_with(tokens: &[(&str, &str)], js: &str) -> String {
    let mut out = js.to_string();
    for (token, value) in tokens {
        out = out.replace(token, value);
    }
    out
}

/// Collect asset paths (`*.js` / `*.wasm` / `*.css`) from an HTML or JS text.
/// Anchored on the extension and walked backward over path characters rather
/// than paired quotes: a lone apostrophe inside a double-quoted script must
/// not shift quote pairing and swallow a modulepreload href.
fn discover_assets(text: &str, urls: &mut std::collections::BTreeSet<String>) {
    let is_path_char = |b: u8| {
        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_')
    };
    let bytes = text.as_bytes();
    for extension in [".js", ".wasm", ".css"] {
        let mut start = 0;
        while let Some(found) = text[start..].find(extension) {
            let end = start + found + extension.len();
            start = end;
            // The extension must terminate the path (next char is a quote or
            // other non-path character).
            if bytes.get(end).is_some_and(|b| is_path_char(*b)) {
                continue;
            }
            let mut begin = end - extension.len();
            while begin > 0 && is_path_char(bytes[begin - 1]) {
                begin -= 1;
            }
            let candidate = &text[begin..end];
            if candidate.starts_with("//")
                || candidate.starts_with("data:")
                || !candidate.bytes().any(|b| b.is_ascii_alphanumeric())
            {
                continue;
            }
            let candidate = candidate.strip_prefix("./").unwrap_or(candidate);
            let normalized = if let Some(stripped) = candidate.strip_prefix('/') {
                format!("/{stripped}")
            } else {
                format!("/{candidate}")
            };
            // Keep only plausible single-file asset paths; a walk that
            // crossed a scheme separator or traversal component is noise.
            if normalized.contains("::") || normalized.split('/').any(|part| part == "..") {
                continue;
            }
            urls.insert(normalized);
        }
    }
}

/// Mirror the app dist from the running server into `dest` over raw HTTP so
/// the second fixture graph-api can serve the identical build via
/// `--assets-dir`. The dist uses stable file names (`filehash = false` in
/// app/Trunk.toml); snippet files are discovered from index.html and the JS
/// glue rather than hardcoded. Raw HTTP keeps 404s off any console listener.
async fn mirror_dist(origin: &str, dest: &Path) -> Result<usize> {
    let mut urls = std::collections::BTreeSet::from([
        "/app.css".to_string(),
        "/jump-cannon-ui.js".to_string(),
        "/jump-cannon-ui_bg.wasm".to_string(),
        "/tvix-worker.js".to_string(),
        "/tvix-worker_bg.wasm".to_string(),
    ]);
    let (status, html) = raw_get(&format!("{origin}/"), &[]).await?;
    if status != 200 {
        bail!("mirror discovery: GET {origin}/ -> HTTP {status}");
    }
    discover_assets(&String::from_utf8_lossy(&html), &mut urls);
    for glue in ["/jump-cannon-ui.js", "/tvix-worker.js"] {
        let (status, js) = raw_get(&format!("{origin}{glue}"), &[]).await?;
        if status == 200 {
            discover_assets(&String::from_utf8_lossy(&js), &mut urls);
        }
    }
    urls.insert("/".to_string()); // index.html itself

    let mut mirrored = 0_usize;
    for path in urls {
        let relative = path.trim_start_matches('/');
        let relative = if relative.is_empty() {
            "index.html"
        } else {
            relative
        };
        let target = dest.join(relative);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (status, bytes) = raw_get(&format!("{origin}{path}"), &[]).await?;
        // Discovery over minified glue yields occasional false positives
        // (string-concatenated fragments); skip anything not served rather
        // than failing the mirror. graph-api's static fallback answers
        // unknown paths with index.html, which mirrors as a harmless copy.
        if status != 200 {
            continue;
        }
        tokio::fs::write(&target, bytes).await?;
        mirrored += 1;
    }
    Ok(mirrored)
}

/// Build the second fixture server: tempdir vaults, mirrored dist, and a
/// spawned graph-api with the switch group configured. Returns `None` when
/// no graph-api binary is available (scenario skipped).
async fn setup_switch_fixture(origin: &str) -> Result<Option<SwitchFixture>> {
    let Some(bin) = find_graph_api_bin() else {
        return Ok(None);
    };
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work_dir = std::env::temp_dir().join(format!("jump-cannon-switch-{}-{unique}", std::process::id()));
    let assets = work_dir.join("assets");
    let default_vault = work_dir.join("default-vault");
    let alt_vault = work_dir.join("alt-vault");
    tokio::fs::create_dir_all(&assets).await?;
    tokio::fs::create_dir_all(&default_vault).await?;
    tokio::fs::create_dir_all(&alt_vault).await?;
    tokio::fs::write(
        default_vault.join(format!("{SWITCH_DEFAULT_NODE}.md")),
        "---\ntitle: Switch Default Fixture\ntags: [browser-switch]\n---\n\nSWITCH_DEFAULT_SENTINEL\n",
    )
    .await?;
    tokio::fs::write(
        alt_vault.join(format!("{SWITCH_ALT_NODE}.md")),
        "---\ntitle: Switch Alt Fixture\ntags: [browser-switch-alt]\n---\n\nSWITCH_ALT_SENTINEL\n",
    )
    .await?;

    let mirrored = mirror_dist(origin, &assets).await.context("mirror app dist")?;
    if mirrored == 0 {
        bail!("mirrored no assets from {origin}");
    }

    let alt_path = alt_vault.to_string_lossy().to_string();
    let catalog = serde_json::json!({
        "selected": SWITCH_DEFAULT_ID,
        "sources": {
            SWITCH_DEFAULT_ID: {
                "displayName": "Switch default vault",
                "description": "Deployment default for the runtime-switch browser contract.",
                "kind": "obsidian"
            },
            SWITCH_ALT_ID: {
                "displayName": "Switch alternate vault",
                "description": "Runnable alternate for the runtime-switch browser contract.",
                "kind": "obsidian",
                "source": {
                    "volumeName": "switch-alt-vault",
                    "existingClaim": "switch-alt-vault",
                    "mountPath": alt_path,
                    "path": alt_path,
                    "readOnly": false
                }
            }
        }
    })
    .to_string();

    let port = pick_free_port().await?;
    let server = tokio::process::Command::new(bin)
        .arg("--vault-root")
        .arg(&default_vault)
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-browser")
        .arg("--assets-dir")
        .arg(&assets)
        .env("JUMP_CANNON_IMPORTER_CATALOG_JSON", catalog)
        .env("JUMP_CANNON_IMPORTER_SWITCH_GROUP", SWITCH_GROUP)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("spawn second fixture graph-api")?;
    let base_url = format!("http://127.0.0.1:{port}");
    probe_server(&format!("{base_url}/"), Duration::from_secs(30)).await?;
    Ok(Some(SwitchFixture {
        base_url,
        server,
        work_dir,
    }))
}

/// Settings > Importers summary: switch posture, policy note, and per-card
/// radio state. Token-free; shared by the authorized and unauthorized pages.
const IMPORTERS_VIEW_JS: &str = r#"(async () => {
    const waitFor = async (predicate, timeoutMs = 30000) => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        const value = predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return null;
    };
    const panel = await waitFor(() => document.querySelector('section.panel-settings'));
    if (!panel) return { error: 'settings panel missing' };
    const tab = [...panel.querySelectorAll('[role="tab"]')]
      .find((candidate) => (candidate.textContent || '').trim() === 'Importers');
    if (!tab) return { error: 'Importers tab missing' };
    tab.click();
    const view = await waitFor(() => panel.querySelector('.importers-view[data-runtime-switch]'));
    if (!view) return { error: 'importers view missing' };
    return {
      switch_state: view.getAttribute('data-runtime-switch'),
      policy: (view.querySelector('.importer-policy')?.textContent || '')
        .replace(/\s+/g, ' ').trim(),
      buttons: [...view.querySelectorAll('.importer-switch-btn')].map((button) => ({
        id: button.getAttribute('data-source-id'),
        viewing: button.getAttribute('data-viewing') === 'true',
        disabled: button.disabled,
      })),
      reset_button: Boolean(view.querySelector('.importer-switch-reset')),
    };
})()"#;

/// Sidebar node ids plus the session-scoped selection. Waits for boot and a
/// populated Nodes navigator.
const NODE_IDS_JS: &str = r#"(async () => {
    const waitFor = async (predicate, timeoutMs = 45000) => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        const value = predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return null;
    };
    const ids = await waitFor(() => {
      if (!window.__jc_boot) return null;
      const rows = [...document.querySelectorAll('[data-testid="node-sidebar"] [data-node-id]')];
      return rows.length ? rows.map((row) => row.getAttribute('data-node-id')) : null;
    });
    return { ids: ids || [], stored: sessionStorage.getItem('jc_source_id') };
})()"#;

/// Click the alternate source's radio and wait for the graph swap: stored
/// selection, alternate-only node list, and the `viewing` badge after the
/// catalog refetch.
const SWITCH_TO_ALT_JS: &str = r#"(async () => {
    const waitFor = async (predicate, timeoutMs = 45000) => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        const value = predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return null;
    };
    const button = await waitFor(() => {
      const candidate = document.querySelector(
        '.importer-switch-btn[data-source-id="__ALT_ID__"]'
      );
      return candidate && !candidate.disabled ? candidate : null;
    });
    if (!button) return { clicked: false, stored: false, swapped: false, badge: false };
    button.click();
    const stored = await waitFor(() =>
      sessionStorage.getItem('jc_source_id') === '__ALT_ID__' ? true : null
    );
    const swapped = await waitFor(() => {
      const ids = [...document.querySelectorAll('[data-testid="node-sidebar"] [data-node-id]')]
        .map((row) => row.getAttribute('data-node-id'));
      // Node ids are source-prefixed (`obsidian:obsidian:<path>`).
      const has = (name) => ids.some((id) => id === name || id.endsWith(':' + name));
      return has('__ALT_NODE__') && !has('__DEFAULT_NODE__') ? true : null;
    });
    const badge = await waitFor(() => document.querySelector(
      '.importer-card[data-source-id="__ALT_ID__"] .importer-badge.viewing'
    ));
    return {
      clicked: true,
      stored: Boolean(stored),
      swapped: Boolean(swapped),
      badge: Boolean(badge),
    };
})()"#;

/// After an in-tab reload the session-scoped selection must persist and the
/// alternate graph must load again.
const PERSISTED_ALT_JS: &str = r#"(async () => {
    const waitFor = async (predicate, timeoutMs = 45000) => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        const value = predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return null;
    };
    const persisted = await waitFor(() => {
      if (!window.__jc_boot) return null;
      if (sessionStorage.getItem('jc_source_id') !== '__ALT_ID__') return null;
      const ids = [...document.querySelectorAll('[data-testid="node-sidebar"] [data-node-id]')]
        .map((row) => row.getAttribute('data-node-id'));
      return ids.some((id) => id === '__ALT_NODE__' || id.endsWith(':__ALT_NODE__'))
        ? true
        : null;
    });
    return { persisted: Boolean(persisted) };
})()"#;

/// Switch back to the deployment default: selection cleared, default nodes.
const SWITCH_BACK_JS: &str = r#"(async () => {
    const waitFor = async (predicate, timeoutMs = 45000) => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        const value = predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return null;
    };
    const panel = await waitFor(() => document.querySelector('section.panel-settings'));
    const tab = panel && [...panel.querySelectorAll('[role="tab"]')]
      .find((candidate) => (candidate.textContent || '').trim() === 'Importers');
    tab?.click();
    const button = await waitFor(() => {
      const candidate = document.querySelector(
        '.importer-switch-btn[data-source-id="__DEFAULT_ID__"]'
      );
      return candidate && !candidate.disabled ? candidate : null;
    });
    if (!button) return { clicked: false, cleared: false, restored: false };
    button.click();
    const cleared = await waitFor(() =>
      sessionStorage.getItem('jc_source_id') === null ? true : null
    );
    const restored = await waitFor(() => {
      const ids = [...document.querySelectorAll('[data-testid="node-sidebar"] [data-node-id]')]
        .map((row) => row.getAttribute('data-node-id'));
      return ids.some((id) => id === '__DEFAULT_NODE__' || id.endsWith(':__DEFAULT_NODE__'))
        ? true
        : null;
    });
    return {
      clicked: true,
      cleared: Boolean(cleared),
      restored: Boolean(restored),
    };
})()"#;

/// Stale-selection recovery on an unauthorized page: the catalog stays
/// reachable (it never requires the group), the reset affordance clears the
/// session selection, and the default graph loads.
const RESET_STALE_JS: &str = r#"(async () => {
    const waitFor = async (predicate, timeoutMs = 45000) => {
      const deadline = performance.now() + timeoutMs;
      while (performance.now() < deadline) {
        const value = predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      return null;
    };
    const button = await waitFor(() => document.querySelector('.importer-switch-reset'));
    if (!button) return { found: false, cleared: false, restored: false };
    button.click();
    const cleared = await waitFor(() =>
      sessionStorage.getItem('jc_source_id') === null ? true : null
    );
    const restored = await waitFor(() => {
      const ids = [...document.querySelectorAll('[data-testid="node-sidebar"] [data-node-id]')]
        .map((row) => row.getAttribute('data-node-id'));
      return ids.some((id) => id === '__DEFAULT_NODE__' || id.endsWith(':__DEFAULT_NODE__'))
        ? true
        : null;
    });
    return {
      found: true,
      cleared: Boolean(cleared),
      restored: Boolean(restored),
    };
})()"#;

/// Navigate a fresh page to `url`, optionally injecting the group header the
/// authenticating proxy would set. Navigation is scheduled (not awaited) for
/// the same reason as the main page: software WebGPU can hold the page-load
/// lifecycle pending past the CDP command deadline.
async fn open_switch_page(
    browser: &Browser,
    url: &str,
    with_group: bool,
) -> Result<chromiumoxide::Page> {
    use chromiumoxide::cdp::browser_protocol::network::{
        Headers, SetExtraHttpHeadersParams,
    };

    let page = browser.new_page("about:blank").await.context("switch page")?;
    if with_group {
        page.execute(SetExtraHttpHeadersParams::new(Headers::new(
            serde_json::json!({ "x-netbird-groups": SWITCH_GROUP }),
        )))
        .await
        .context("set extra HTTP headers")?;
    }
    let target = serde_json::to_string(url)?;
    page.evaluate(format!(
        "setTimeout(() => window.location.replace({target}), 0)"
    ))
    .await
    .context("schedule switch page navigation")?;
    Ok(page)
}

/// Evaluate with retries: in-tab reloads destroy the JS execution context,
/// and a racing evaluate fails rather than returning stale state.
async fn evaluate_retry<T: serde::de::DeserializeOwned>(
    page: &chromiumoxide::Page,
    js: &str,
    attempts: usize,
) -> Result<T> {
    let mut last_error = None;
    for _ in 0..attempts {
        match page.evaluate(js).await {
            Ok(value) => return Ok(value.into_value().context("decode evaluation result")?),
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    Err(anyhow!("evaluate failed after {attempts} attempts: {:?}", last_error))
}

/// Node ids are source-prefixed (`obsidian:obsidian:<path>`); match a
/// fixture by exact id or `:<name>` suffix.
fn node_id_matches(id: &str, name: &str) -> bool {
    id == name || id.ends_with(&format!(":{name}"))
}

/// Drive the runtime-switch contract against the second fixture server.
async fn run_switch_scenario(
    browser: &Browser,
    fixture: SwitchFixture,
    console_logs: Arc<Mutex<Vec<String>>>,
) -> ImporterSwitchCheck {
    let mut check = ImporterSwitchCheck {
        ok: false,
        skipped: false,
        wire_forbidden: false,
        wire_authorized: false,
        wire_unknown: false,
        selector_visible: false,
        switch_swaps_nodes: false,
        session_persists_reload: false,
        switch_back_restores_default: false,
        fresh_tab_default: false,
        denied_note: false,
        stale_reset_recovers: false,
        viewing_non_default_reset: false,
        reason: None,
    };

    if let Err(error) = run_switch_scenario_inner(browser, &fixture, &mut check, console_logs).await
    {
        check.reason = Some(match check.reason.take() {
            Some(reason) => format!("{reason}; {error:#}"),
            None => format!("{error:#}"),
        });
    }
    check.ok = check.reason.is_none()
        && check.wire_forbidden
        && check.wire_authorized
        && check.wire_unknown
        && check.selector_visible
        && check.switch_swaps_nodes
        && check.session_persists_reload
        && check.switch_back_restores_default
        && check.fresh_tab_default
        && check.stale_reset_recovers
        && check.viewing_non_default_reset;
    if !check.ok && check.reason.is_none() {
        check.reason = Some("one or more importer-switch assertions failed".to_string());
    }
    // kill_on_drop reaps the fixture server; remove the vault/asset tempdir.
    drop(fixture.server);
    let _ = std::fs::remove_dir_all(&fixture.work_dir);
    check
}

async fn run_switch_scenario_inner(
    browser: &Browser,
    fixture: &SwitchFixture,
    check: &mut ImporterSwitchCheck,
    console_logs: Arc<Mutex<Vec<String>>>,
) -> Result<()> {
    let base = &fixture.base_url;

    // ---- wire contract (no browser): the group gate and id validation ----
    let source = ("x-jump-cannon-source", SWITCH_ALT_ID);
    check.wire_forbidden = raw_get_status(&format!("{base}/graph/ids"), &[source]).await? == 403;
    check.wire_authorized = raw_get_status(
        &format!("{base}/graph/ids"),
        &[source, ("x-netbird-groups", SWITCH_GROUP)],
    )
    .await?
        == 200;
    check.wire_unknown = raw_get_status(
        &format!("{base}/graph/ids"),
        &[
            ("x-jump-cannon-source", "no-such-source"),
            ("x-netbird-groups", SWITCH_GROUP),
        ],
    )
    .await?
        == 404;

    // ---- authorized page: selector, switch, persistence, switch-back ------
    let page = open_switch_page(browser, base, true).await?;
    // Feed the authorized page's console into the shared error gate: the
    // happy path must stay clean.
    let mut page_logs = page
        .event_listener::<chromiumoxide::cdp::browser_protocol::log::EventEntryAdded>()
        .await
        .context("listen switch page log entries")?;
    let logs_a = console_logs.clone();
    let page_log_pump = tokio::spawn(async move {
        while let Some(ev) = page_logs.next().await {
            let line = format!("[{}] {}", ev.entry.level.as_ref(), ev.entry.text);
            logs_a.lock().await.push(line);
        }
    });
    let mut page_exceptions = page
        .event_listener::<chromiumoxide::cdp::js_protocol::runtime::EventExceptionThrown>()
        .await
        .context("listen switch page exceptions")?;
    let logs_b = console_logs;
    let page_exception_pump = tokio::spawn(async move {
        while let Some(ev) = page_exceptions.next().await {
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
            logs_b.lock().await.push(line);
        }
    });

    let view: serde_json::Value = evaluate_retry(&page, IMPORTERS_VIEW_JS, 5).await?;
    let buttons = view
        .get("buttons")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default();
    let button_for = |id: &str| {
        buttons.iter().find(|b| b.get("id").and_then(|v| v.as_str()) == Some(id))
    };
    check.selector_visible = view.get("switch_state").and_then(|v| v.as_str()) == Some("enabled")
        && button_for(SWITCH_ALT_ID)
            .is_some_and(|b| b.get("disabled").and_then(|v| v.as_bool()) == Some(false))
        && button_for(SWITCH_DEFAULT_ID)
            .is_some_and(|b| b.get("viewing").and_then(|v| v.as_bool()) == Some(true));

    let initial: serde_json::Value = evaluate_retry(&page, NODE_IDS_JS, 5).await?;
    let initial_ids = initial
        .get("ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let initial_default = initial_ids
        .iter()
        .any(|id| id.as_str().is_some_and(|id| node_id_matches(id, SWITCH_DEFAULT_NODE)));

    let switched: serde_json::Value = evaluate_retry(
        &page,
        &js_with(
            &[
                ("__ALT_ID__", SWITCH_ALT_ID),
                ("__ALT_NODE__", SWITCH_ALT_NODE),
                ("__DEFAULT_NODE__", SWITCH_DEFAULT_NODE),
            ],
            SWITCH_TO_ALT_JS,
        ),
        5,
    )
    .await?;
    check.switch_swaps_nodes = initial_default
        && switched.get("clicked").and_then(|v| v.as_bool()) == Some(true)
        && switched.get("stored").and_then(|v| v.as_bool()) == Some(true)
        && switched.get("swapped").and_then(|v| v.as_bool()) == Some(true)
        && switched.get("badge").and_then(|v| v.as_bool()) == Some(true);
    // While viewing the alternate (still authorized), the policy reset
    // affordance must be visible and clicking it must clear the session
    // selection and restore the deployment default. Re-runs SWITCH_TO_ALT
    // below so the reload test still exercises the alternate path.
    let alt_view: serde_json::Value = evaluate_retry(&page, IMPORTERS_VIEW_JS, 5).await?;
    let reset_visible = alt_view.get("reset_button").and_then(|v| v.as_bool()) == Some(true);
    let reset: serde_json::Value = evaluate_retry(
        &page,
        &js_with(&[("__DEFAULT_NODE__", SWITCH_DEFAULT_NODE)], RESET_STALE_JS),
        5,
    )
    .await?;
    check.viewing_non_default_reset = reset_visible
        && reset.get("found").and_then(|v| v.as_bool()) == Some(true)
        && reset.get("cleared").and_then(|v| v.as_bool()) == Some(true)
        && reset.get("restored").and_then(|v| v.as_bool()) == Some(true);
    let _: () = evaluate_retry(
        &page,
        &js_with(
            &[("__ALT_ID__", SWITCH_ALT_ID), ("__ALT_NODE__", SWITCH_ALT_NODE)],
            SWITCH_TO_ALT_JS,
        ),
        5,
    )
    .await?;

    // In-tab reload: the session selection survives and re-loads the
    // alternate graph.
    let _ = page.evaluate("location.reload()").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let persisted: serde_json::Value = evaluate_retry(
        &page,
        &js_with(
            &[
                ("__ALT_ID__", SWITCH_ALT_ID),
                ("__ALT_NODE__", SWITCH_ALT_NODE),
            ],
            PERSISTED_ALT_JS,
        ),
        10,
    )
    .await?;
    check.session_persists_reload =
        persisted.get("persisted").and_then(|v| v.as_bool()) == Some(true);

    let switched_back: serde_json::Value = evaluate_retry(
        &page,
        &js_with(
            &[
                ("__DEFAULT_ID__", SWITCH_DEFAULT_ID),
                ("__DEFAULT_NODE__", SWITCH_DEFAULT_NODE),
            ],
            SWITCH_BACK_JS,
        ),
        5,
    )
    .await?;
    check.switch_back_restores_default =
        switched_back.get("clicked").and_then(|v| v.as_bool()) == Some(true)
            && switched_back.get("cleared").and_then(|v| v.as_bool()) == Some(true)
            && switched_back.get("restored").and_then(|v| v.as_bool()) == Some(true);

    page_log_pump.abort();
    page_exception_pump.abort();
    let _ = page_log_pump.await;
    let _ = page_exception_pump.await;
    let _ = page.close().await;

    // ---- fresh tab, authorized: session storage is per-tab, so the default
    // view returns even though another tab selected the alternate.
    let fresh = open_switch_page(browser, base, true).await?;
    let fresh_state: serde_json::Value = evaluate_retry(&fresh, NODE_IDS_JS, 5).await?;
    let fresh_ids = fresh_state
        .get("ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    check.fresh_tab_default = fresh_state.get("stored").and_then(|v| v.as_null()).is_some()
        && fresh_ids
            .iter()
            .any(|id| id.as_str().is_some_and(|id| node_id_matches(id, SWITCH_DEFAULT_NODE)));
    let _ = fresh.close().await;

    // ---- unauthorized page: the group-required note, no selector, and
    // stale-selection recovery (no group header → no extra console gating;
    // the page deliberately fetches a 403).
    let denied = open_switch_page(browser, base, false).await?;
    let denied_view: serde_json::Value = evaluate_retry(&denied, IMPORTERS_VIEW_JS, 5).await?;
    let denied_policy = denied_view
        .get("policy")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let denied_buttons = denied_view
        .get("buttons")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    check.denied_note = denied_view.get("switch_state").and_then(|v| v.as_str()) == Some("denied")
        && denied_buttons.is_empty()
        && denied_policy.contains("Switching requires NetBird group")
        && denied_policy.contains(SWITCH_GROUP);

    // Plant a stale selection as if the viewer had switched before losing
    // the group, reload, and recover through the reset affordance.
    denied
        .evaluate("sessionStorage.setItem('jc_source_id', 'alt-obsidian'); location.reload()")
        .await
        .ok();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let stale_view: serde_json::Value = evaluate_retry(&denied, IMPORTERS_VIEW_JS, 10).await?;
    let reset_visible = stale_view
        .get("reset_button")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let reset: serde_json::Value = evaluate_retry(
        &denied,
        &js_with(&[("__DEFAULT_NODE__", SWITCH_DEFAULT_NODE)], RESET_STALE_JS),
        5,
    )
    .await?;
    check.stale_reset_recovers = reset_visible
        && reset.get("found").and_then(|v| v.as_bool()) == Some(true)
        && reset.get("cleared").and_then(|v| v.as_bool()) == Some(true)
        && reset.get("restored").and_then(|v| v.as_bool()) == Some(true);
    let _ = denied.close().await;

    Ok(())
}

fn tail(v: &[String], n: usize) -> Vec<String> {
    let start = v.len().saturating_sub(n);
    v[start..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::{browser_log_is_error, chromium_args, raw_http_probe_required};

    #[test]
    fn chromiumoxide_arguments_are_keys_not_cli_tokens() {
        let args = chromium_args();
        assert!(!args.is_empty());
        assert!(args.iter().all(|arg| !arg.starts_with('-')));
        assert!(args.iter().all(|arg| !arg.contains('=')));
    }

    #[test]
    fn browser_errors_cover_cdp_and_rust_tracing_levels() {
        assert!(browser_log_is_error("[error] Failed to load resource"));
        assert!(browser_log_is_error("[exception] Uncaught TypeError"));
        assert!(browser_log_is_error(
            "[log] \"ERROR ui/src/render/mod.rs: wgpu init failed\""
        ));
        assert!(browser_log_is_error(
            "[log] \"%cERROR%c ui/src/render/mod.rs: wgpu init failed\" \"color: red\" \"\""
        ));
        assert!(!browser_log_is_error("[warning] preload warning"));
        assert!(!browser_log_is_error("[log] \"[jump-cannon-ui] boot\""));
    }

    #[test]
    fn production_https_uses_chromiums_network_stack() {
        assert!(raw_http_probe_required("http://127.0.0.1:8765/").unwrap());
        assert!(!raw_http_probe_required("https://jump-cannon.example/").unwrap());
        assert!(raw_http_probe_required("file:///tmp/index.html").is_err());
    }
}
