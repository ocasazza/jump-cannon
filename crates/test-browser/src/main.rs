//! Rust-driven browser regression suite.
//!
//! Asserts the bare minimum that future regression checks will build on:
//!
//!   1. The page at `--base-url` responds (HTTP 200).
//!   2. Headless Chromium launches with WebGPU flags and navigates.
//!   3. The boot log line `[jump-cannon-ui] boot` appears on the JS
//!      console within `--timeout-secs`.
//!   4. The graph canvas becomes render-ready and its header controls work.
//!   5. Nodes is a two-pane editor; Flat/Tags selection and content work.
//!   6. Unified Settings exposes four accessible, content-backed tabs.
//!   7. Filter is a repeatable, nested Boolean builder with live validation.
//!   8. The Sessions view switcher mounts the world workspace (Worlds panel,
//!      dock) against the embedded host and returns to the User view.
//!   9. Screenshots are saved for the Nodes editor, Filter builder, Sessions
//!      view, and workspace.
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
    graph_header_actions: Option<HeaderActionCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes_editor: Option<NodesEditorCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_tabs: Option<SettingsTabsCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter_builder: Option<FilterBuilderCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions_view: Option<SessionsViewCheck>,
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
    importer_no_mutation_controls: bool,
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
        header_actions,
        nodes_editor,
        settings_tabs,
        filter_builder,
        sessions_view,
        page_errors,
    ) = match &result {
        Ok(o) => {
            let ok = o.boot_log_found
                && o.canvas_width > 0
                && o.canvas_height > 0
                && o.header_actions.ok
                && o.nodes_editor.ok
                && o.settings_tabs.ok
                && o.filter_builder.ok
                && o.sessions_view.ok
                && captured_page_errors.is_empty();
            let reason = if !o.boot_log_found {
                Some(format!("boot log {BOOT_LOG_NEEDLE:?} was not observed"))
            } else if o.canvas_width == 0 || o.canvas_height == 0 {
                Some(format!(
                    "canvas dimensions invalid: {}x{}",
                    o.canvas_width, o.canvas_height
                ))
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
                Some(o.header_actions.clone()),
                Some(o.nodes_editor.clone()),
                Some(o.settings_tabs.clone()),
                Some(o.filter_builder.clone()),
                Some(o.sessions_view.clone()),
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
        graph_header_actions: header_actions,
        nodes_editor,
        settings_tabs,
        filter_builder,
        sessions_view,
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
    nodes_editor: NodesEditorCheck,
    settings_tabs: SettingsTabsCheck,
    filter_builder: FilterBuilderCheck,
    sessions_view: SessionsViewCheck,
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
            .some((node) => node.getAttribute('data-node-id') === 'Node Shared Fixture')
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
            .some((node) => node.getAttribute('data-node-id') === 'Node Untagged Fixture')
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
        let importerNoMutationControls = false;

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
            const selectedProfile = text('[data-field="selected-profile"]');
            const activeKind = text('[data-field="active-kind"]');
            const knownKinds = new Set([
              'obsidian', 'tvix', 'generate', 'kubernetes', 'okf', 'pest'
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
            importerCatalog = Boolean(
              content.querySelector('.importers-view[data-activation="helm_rollout"]') &&
              lavender &&
              selectionMatches &&
              text('[data-field="active-importer-id"]') &&
              /Configured by Helm/i.test(policy) &&
              /rollout is required/i.test(policy)
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
            const runtimeControls = [...content.querySelectorAll('button, input, select, textarea')]
              .filter((control) => /apply|run|activate|switch/i.test(
                `${control.textContent || ''} ${control.getAttribute('aria-label') || ''}`
              ));
            importerNoMutationControls = runtimeControls.length === 0;
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
        if (!importerNoMutationControls) failures.push('Importer catalog exposes a runtime mutation control');
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
          importer_no_mutation_controls: Boolean(importerNoMutationControls),
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
    // geometry, and drive only its stable test/ARIA contract. The fixture tags
    // make the nested group's ALL/ANY counts deterministic: their intersection
    // is one node and their union is two nodes.
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
        await setFieldRule(editorFieldId, 'tags', 'browser-editor');
        await setFieldRule(sharedFieldId, 'tags', 'browser-shared');

        const allReady = await waitFor(() => {
          const group = groupById(nestedId);
          return group?.getAttribute('data-mode') === 'all' && countOf(group) === 1 && group;
        });
        const allCount = allReady ? countOf(allReady) : 0;
        own(groupById(nestedId), 'filter-group-mode')?.parentElement
          ?.querySelector('[data-testid="filter-group-mode"][data-mode-target="any"]')
          ?.click();
        const anyReady = await waitFor(() => {
          const group = groupById(nestedId);
          return group?.getAttribute('data-mode') === 'any' && countOf(group) === 2 && group;
        });
        const anyCount = anyReady ? countOf(anyReady) : 0;
        const modeCounts = allCount === 1 && anyCount === 2;
        fieldRules = ownRules(groupById(nestedId), 'field');
        const fieldValues = fieldRules.map((rule) =>
          rule.querySelector(selector('filter-field-value'))?.value || ''
        );
        const fieldsPresent = fieldRules.length === 2 &&
          fieldValues.includes('browser-editor') && fieldValues.includes('browser-shared') &&
          fieldRules.every((rule) =>
            (rule.querySelector(selector('filter-field-name'))?.value || '') === 'tags' &&
            Boolean(rule.getAttribute('data-expression'))
          );

        const matchesOperator = await setMatchesOperator(editorFieldId);
        await setValue(
          ruleById(editorFieldId)?.querySelector(selector('filter-field-value')),
          'browser-editor'
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
        const inlineDiagnostic = Boolean(matchesOperator && diagnostic);
        const lastValidState = Boolean(
          appliedBeforeInvalid && invalidEvaluation?.getAttribute('data-phase') === 'invalid' &&
          invalidEvaluation?.getAttribute('data-applied-count') === lastAppliedCount &&
          countOf(groupById(nestedId)) === anyCount
        );

        if (!dockChip || !initialPanel || !maximize || !panelVisible) {
          failures.push('Filter did not restore from the dock and maximize');
        }
        if (!independentSearchRules) failures.push('repeatable Search rules are not independent');
        if (!accessibleReorder) failures.push('Search rules did not expose a working accessible reorder');
        if (!fieldsPresent) failures.push('fixture tag field rules are incomplete');
        if (!modeCounts) failures.push('nested ALL/ANY counts were not 1 and 2');
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
    // View switches RELOAD the page (persisted `jc_view` + location.reload),
    // so each click destroys the JS execution context: assertions must run in
    // fresh evaluates against the reloaded page, detecting the new runtime by
    // the `window.__jc_boot` stamp changing. With no session-manager URL
    // configured the app boots the embedded single-user host, so the contract
    // is deterministic: Worlds panel with its empty state, dock present, and
    // switching back to User remounts the graph canvas.
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
        return { switch_found: switchFound, boot_before: window.__jc_boot || 0 };
    })()"#;
    let click_value: serde_json::Value = page.evaluate(click_sessions_js).await?.into_value()?;
    let switch_found = click_value
        .get("switch_found")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let boot_before = click_value
        .get("boot_before")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    // Poll (in fresh evaluates) until the reloaded page reports a new boot
    // stamp with the Sessions button active.
    let sessions_state_js = r#"(() => {
        const buttons = [...(document.querySelector('nav.view-switch')
          ?.querySelectorAll('button.view-btn') || [])];
        const sessions = buttons.find(
          (button) => (button.textContent || '').trim() === 'Sessions'
        );
        return {
          boot: window.__jc_boot || 0,
          active: Boolean(sessions?.classList.contains('active')),
        };
    })()"#;
    let mut sessions_active = false;
    for _ in 0..50 {
        // The page reloads on switch: evaluates can race the navigation and
        // lose their execution context — retry rather than fail.
        let Ok(eval) = page.evaluate(sessions_state_js).await else {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            continue;
        };
        let v: serde_json::Value = eval.into_value()?;
        let boot = v.get("boot").and_then(|b| b.as_f64()).unwrap_or(0.0);
        let active = v.get("active").and_then(|a| a.as_bool()).unwrap_or(false);
        if boot != boot_before && active {
            sessions_active = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
    // (a second reload) and keeps any later assertions on the default surface.
    let click_user_js = r#"(() => {
        const buttons = [...(document.querySelector('nav.view-switch')
          ?.querySelectorAll('button.view-btn') || [])];
        const user = buttons.find(
          (button) => (button.textContent || '').trim() === 'User'
        );
        user?.click();
        return { boot_before: window.__jc_boot || 0 };
    })()"#;
    let click_user_value: serde_json::Value =
        page.evaluate(click_user_js).await?.into_value()?;
    let user_boot_before = click_user_value
        .get("boot_before")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let user_state_js = r#"(() => {
        const buttons = [...(document.querySelector('nav.view-switch')
          ?.querySelectorAll('button.view-btn') || [])];
        const user = buttons.find(
          (button) => (button.textContent || '').trim() === 'User'
        );
        const canvas = document.querySelector('section.panel-graph canvas.graph-canvas');
        return {
          boot: window.__jc_boot || 0,
          active: Boolean(user?.classList.contains('active')),
          graph_ready: canvas?.dataset.renderReady === 'true' &&
            Number(canvas?.dataset.nodeCount || 0) > 0,
        };
    })()"#;
    let mut user_restored = false;
    for _ in 0..50 {
        let Ok(eval) = page.evaluate(user_state_js).await else {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            continue;
        };
        let v: serde_json::Value = eval.into_value()?;
        let boot = v.get("boot").and_then(|b| b.as_f64()).unwrap_or(0.0);
        let active = v.get("active").and_then(|a| a.as_bool()).unwrap_or(false);
        let graph_ready = v
            .get("graph_ready")
            .and_then(|g| g.as_bool())
            .unwrap_or(false);
        if boot != user_boot_before && active && graph_ready {
            user_restored = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    sessions_view.user_restored = user_restored;
    if !user_restored {
        sessions_view.ok = false;
        sessions_view.reason = Some(match sessions_view.reason.take() {
            Some(reason) => format!("{reason}; User view did not restore a render-ready graph"),
            None => "User view did not restore a render-ready graph".to_string(),
        });
    }

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
        header_actions,
        nodes_editor,
        settings_tabs,
        filter_builder,
        sessions_view,
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
