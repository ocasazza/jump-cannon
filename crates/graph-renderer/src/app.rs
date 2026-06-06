use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::data::{self, Bootstrap, LoadState, SharedLoad};
use crate::fetch::ApiClient;
use crate::graph_pipelines::{GraphData, GraphPipelines};
use crate::perf::{PerfCollector, StageId};
use crate::proto;
use crate::ui;
use crate::ui::actions::{self, ActionRegistry, BuiltinAction, ParamValue};
use crate::ui::command_palette::PaletteOutcome;
use crate::ui::input::{default_bindings, AppAction};
use jump_io::{adapter::egui_raw, Event as InputEvent, InputCtx};
use crate::ui::field_index::FieldIndex;
use crate::ui::focus_set::{self, FocusCtx, FocusMode};
use crate::ui::layout::registry::LayoutRegistry;
use crate::ui::progress::{Progress, ProgressSink};
use crate::ui::query::EvalContext;
use crate::ui::state::{ColorBy, EdgeColorBy, FontFamilyChoice, ShapeBy, SizeBy};
use graph_layouts::{warmup_positions, GpuForceOptions, SeedMode};

/// Translate a UI multiplier slider value into the actual scalar applied
/// downstream. When `log_scale` is on, the slider is interpreted as
/// `10^(value - 1.0)` so 1.0 → 1.0×, 2.0 → 10×, 0.0 → 0.1×. When off,
/// the slider value is returned as-is.
#[inline]
fn apply_size_scale(slider: f32, log_scale: bool) -> f32 {
    if log_scale {
        10f32.powf(slider - 1.0)
    } else {
        slider
    }
}

/// Result of an async `/node/:id` fetch.
///
/// Outer `Option` = "did a fetch complete since the last poll?". When the
/// outer is `Some`, the inner `Result` is the fetch outcome:
///   * `Ok(Some(meta))` — server returned a NodeMeta.
///   * `Ok(None)`       — server returned 404 (id legitimately not in the
///     in-memory graph and no Prisma fallback configured). Soft outcome:
///     the renderer just skips opening the modal instead of logging.
///   * `Err(e)`         — actual transport / decode failure.
type NodeFetchSlot = Arc<Mutex<Option<Result<Option<proto::NodeMeta>, String>>>>;

/// Shared cache of `/search?q=` results keyed by the query string.
/// Async tasks push into it; the evaluator reads from it.
type SearchCache = Arc<Mutex<HashMap<String, HashSet<u32>>>>;

pub struct App {
    state: ui::AppState,
    load: SharedLoad,
    /// Once we successfully push a Bootstrap into GraphPipelines we flip
    /// this so we don't retry the (expensive) buffer creation.
    loaded_into_gpu: bool,
    /// One-shot request to `fit_camera()` after the next successful GPU
    /// load. Set by `drain_generated_graph` so a regenerated graph is
    /// framed in view (the freshly-loaded graph has new bounds and the
    /// idle auto-refit only fires on a window resize). Cleared after the
    /// fit fires.
    pending_fit_on_load: bool,
    /// Set once we emit the readiness console-log line (used by the test harness).
    logged_ready: bool,
    /// Phase E: ephemeral modal state. Not persisted — open-state is per-session.
    modal: ui::ModalState,
    /// Async slot the fetch task writes the NodeMeta into.
    node_fetch: NodeFetchSlot,
    /// In-flight Generate (tvix) evaluation, if any. Spawned from
    /// `state.generate.request` so `tvix_wasm::eval_graph` runs OFF the
    /// click-handler (native: a real thread; WASM: paint-first-then-run).
    /// Polled each frame by `drain_generated_graph`; the `Ok` graph lands in
    /// `state.generate.pending` for the existing promotion path.
    generate_job: Option<crate::job::BackgroundJob<tvix_wasm::GeneratedGraph>>,
    /// Cached idx -> string id table (from Bootstrap.ids), used to resolve
    /// raycast hits to node ids. Populated when bootstrap promotes to GPU.
    ids: Vec<String>,
    /// Reverse map id -> idx, computed once on bootstrap promotion.
    id_to_idx: HashMap<String, u32>,
    /// Per-node metric buffers (degree, pagerank, community, kcore, …)
    /// kept on the host so style changes can recompute color/size buffers
    /// without a re-fetch.
    metrics: HashMap<String, Vec<f32>>,
    /// Base URL for `/node/:id` follow-up fetches.
    base_url: String,
    /// Whether graph-api is currently reachable. Set by the long-lived
    /// compute-health watcher (an `Ok` reply means graph-api answered; an `Err`
    /// means it's down / the route 404s). Drives the `GenerateBackendChoice::Auto`
    /// default: Server when reachable, else a local fallback. Starts `true`
    /// (optimistic) — the first probe corrects it within ~2s.
    server_reachable: Arc<Mutex<bool>>,
    /// Async-shared `/search?q=` result cache.
    search_cache: SearchCache,
    /// Search queries we've already kicked off (avoid double-fire).
    search_inflight: HashSet<String>,

    // -- "previous-frame" trackers used to gate GPU writes -----------------
    prev_style_key: Option<(
        SizeBy,
        ColorBy,
        ShapeBy,
        u32,
        u32,
        EdgeColorBy,
        [u32; 4],
        crate::data::PaletteId,
        crate::ui::state::CommunitySource,
    )>,
    prev_layout_key: Option<u64>,
    /// Last-applied gpu-force seed mode. `set_options` only updates the
    /// option struct in place — it does **not** re-run `precompute`, so a
    /// pure settings push can't actually re-seed the layout. To make the
    /// SeedMode combo in the sidebar functional we detect a change here
    /// and force a `swap_physics_layout` (which rebuilds GpuState and
    /// re-runs the seeder) instead of the cheap settings-only push.
    prev_seed_mode: Option<SeedMode>,
    prev_focus_key: Option<u64>,
    prev_cursor_key: Option<u64>,
    prev_selected_hash: Option<u64>,

    // -- input deltas for the camera ---------------------------------------
    prev_canvas_rect: Option<egui::Rect>,
    last_pointer_in_canvas: Option<egui::Pos2>,
    cursor_force_active: f32, // sign: +1 attract / -1 repel / 0 none
    /// Previous-frame value of `cursor_force_active`, used to detect the
    /// release edge (non-zero → 0) so we can kick a brief accelerated
    /// cool-down. Without this, every click wakes the sim and the
    /// HALT_GRACE_STEPS window pins continuous repaint for ~5s.
    prev_cursor_force_active: f32,
    /// Frames remaining in the post-click accelerated-cool-down window.
    /// While > 0 we push a temporary options snapshot with stronger
    /// cooling so the brief perturbation halts fast.
    post_click_cooldown_frames: u32,
    /// Have we already pushed the perturbed (high-cooling) opts for the
    /// active cooldown window? Re-pushing them every frame compounds the
    /// `cooling_alpha *= 0.95` and `energy_threshold *= 5.0` mutations
    /// (since `layout_options()` reads back the *current* opts), and —
    /// because those are non-cursor fields — re-trips `set_options`'s
    /// wake-gating every frame, defeating the cooldown's whole purpose.
    /// Apply once on the rising edge, then leave the in-pipeline opts
    /// alone until `apply_layout_to_gpu` restores user values at the
    /// trailing edge.
    post_click_cooldown_applied: bool,
    /// Latest max-KE readback mirrored from GraphPipelines, used to
    /// pick render cadence (high KE → throttle repaint to ~20fps since
    /// the user can't visually parse 60fps of layout shuffle).
    last_observed_max_ke: f32,

    // -- command palette ---------------------------------------------------
    palette_state: ui::CommandPaletteState,
    actions: ActionRegistry,
    /// Async slot for the palette's preview-fetch (separate from
    /// `node_fetch` which feeds the modal). Holds (id, Result<NodeMeta>).
    palette_preview_slot: Arc<Mutex<Option<(String, Result<proto::NodeMeta, String>)>>>,
    /// Ids the palette has already requested previews for, to avoid
    /// re-spawning fetches every frame while one is in-flight.
    palette_inflight: HashSet<String>,

    // -- layout registry ---------------------------------------------------
    /// Registry of available layout algorithms. Step 1: gpu-force only.
    /// Step 3 will register additional static + physics backends here.
    layout_registry: LayoutRegistry,
    /// Tracks the previously-active layout id so `apply_layout_to_gpu`
    /// can detect a swap and call into `swap_physics_layout`. Step 1
    /// only registers one factory so this never observes a change.
    prev_active_layout_id: Option<String>,

    // -- auto-fit dedup ----------------------------------------------------
    /// Last canvas size we ran `fit_camera()` for. Auto-refit only fires
    /// when this changes (window resize). Following live graph bounds
    /// caused click-blackouts: the cursor force perturbs the sim, bounds
    /// spike, refit zooms way out, sub-pixel cull blanks the screen.
    /// Manual refit via `F`, the Camera section button, or Ctrl+P → Fit
    /// Camera covers the rest.
    last_fit_screen: Option<egui::Vec2>,

    /// Persistent ease-in timer for WASDQE pan speed (seconds of
    /// continuous input). Resets to 0 on the first frame with no pan key
    /// held so a quick tap stays a tap. Threaded into `WorkspaceCtx`.
    camera_pan_accel_t: f32,

    /// Per-frame perf ring buffer (FPS, frame ms, per-stage ms, KE).
    /// Surfaced in the Debug sidebar section.
    pub perf: PerfCollector,

    /// Currently-selected node idx for the right-hand inspector panel.
    /// Session-only — not persisted.
    selected_node_idx: Option<u32>,

    // -- focus mode (hover/click highlight + community dim) ---------------
    /// Sticky focused node from a click. Click on empty canvas clears.
    focus_sticky_idx: Option<u32>,
    /// Transient hover focus. Lives only while the cursor is over a node;
    /// released after `HOVER_HOLD_MS` of no hover. Sticky wins over hover.
    focus_hover_idx: Option<u32>,
    /// Transient hover focus for edges. Mutually exclusive with
    /// `focus_hover_idx` — node hover takes priority. Drives the edge
    /// shader's hover treatment (brighter color + full alpha).
    focus_hover_edge_idx: Option<u32>,
    /// Last frame's focused-into-GPU node idx. Drives change detection
    /// so we only re-upload the dim_alpha buffer when the membership
    /// would actually differ.
    focus_pushed_idx: Option<u32>,
    focus_pushed_mode: Option<FocusMode>,
    /// Hash signature of the last `active_filters` set pushed to the
    /// `dim_alpha` buffer via `set_filter_mask`. Lets `apply_focus_set_to_gpu`
    /// skip GPU re-uploads on idle frames when neither focus nor filters
    /// changed. `None` = nothing pushed yet (forces first write).
    filter_pushed_sig: Option<u64>,
    /// Last hovered-node index handed to the shader (`set_hovered_node`).
    /// Change-detect so we don't write the effects uniform on every
    /// frame the cursor sits still over the same node.
    hovered_pushed_idx: Option<u32>,
    /// Last hovered-edge index handed to the shader (`set_hovered_edge`).
    /// Change-detect mirror of `focus_hover_edge_idx`.
    hovered_pushed_edge_idx: Option<u32>,
    /// Saved [`FocusMode`] from the moment `active_filters` transitioned
    /// empty→non-empty via a badge click. Restored when the filter set
    /// drains back to empty so the user lands back on whatever mode they
    /// had selected before the auto-flip. Session-only, never persisted —
    /// an app reload starts with no snapshot, so a persisted non-empty
    /// filter set won't trigger a phantom restore.
    previous_focus_mode: Option<FocusMode>,
    /// Tracks whether `active_filters` was non-empty on the previous
    /// frame. Drives empty→non-empty (auto-flip) and non-empty→empty
    /// (restore) transitions regardless of *which* surface mutated the
    /// filter set (inspector badges, modal badges, filter chip strip,
    /// query builder etc).
    prev_filters_non_empty: bool,
    /// Throttle: most recent hover-raycast timestamp.
    last_hover_raycast_at: Option<web_time::Instant>,
    /// Hover release timer — once hover goes empty, hold the previous
    /// focus for ~HOVER_HOLD_MS before clearing.
    hover_clear_at: Option<web_time::Instant>,

    // -- Hover-preview card (delayed) ------------------------------------
    //
    // A small floating preview opens after the cursor lingers on a node
    // for `HOVER_PREVIEW_DELAY_MS`. Distinct from `focus_hover_idx` —
    // that drives the per-node highlight; the preview card needs the
    // metadata (title / tags / body) from `/node/:id`, so it's stateful.
    /// Node idx the preview is currently armed/showing for. Resets to
    /// `None` whenever the cursor leaves a node or moves to a different
    /// one.
    hover_preview_idx: Option<u32>,
    /// When the cursor first landed on `hover_preview_idx`. The card
    /// opens once elapsed > `HOVER_PREVIEW_DELAY_MS`.
    hover_preview_armed_at: Option<web_time::Instant>,
    /// Cached NodeMeta + the id it was fetched for. Reused across
    /// rapid hover-on/off cycles on the same node so we don't re-fire
    /// `/node/:id` every flick.
    hover_preview_meta: Option<proto::NodeMeta>,
    /// Async slot for the in-flight /node/:id fetch.
    hover_preview_fetch: Arc<Mutex<Option<Result<Option<proto::NodeMeta>, String>>>>,
    /// True once the preview has been promoted from "armed" to "shown."
    /// Distinct from `hover_preview_idx` because the idx is set
    /// immediately on hover; the visibility flag flips only after the
    /// delay elapses + the fetch returns.
    hover_preview_open: bool,
    /// Canvas-space cursor position when the preview was opened —
    /// drives the floating Area's anchor. Persists between frames so a
    /// jittery cursor doesn't make the card jitter.
    hover_preview_pos: Option<egui::Pos2>,
    /// EMA of projected screen positions per node idx, used to smooth
    /// out 1-frame jitter from the force-sim when an anchored panel is
    /// pinned to a node. Entries grow per hovered node and only decay
    /// when the App is recreated. The map is tiny in practice.
    last_anchored_screen_pos: HashMap<u32, egui::Pos2>,
    /// Sticky-promoted anchored panel idx. Set on click; cleared on X.
    /// Distinct from `selected_node_idx` so promoted previews can
    /// coexist with a different inspector selection.
    promoted_anchored_idx: Option<u32>,
    /// Per-node collapsed flag for the promoted-node `FloatingPanel`.
    /// Toggled by the yellow traffic light. Keyed by node idx (the
    /// promoted panel is session-scoped, like `promoted_anchored_idx`).
    /// Not serialized.
    node_panel_collapsed: HashMap<u32, bool>,
    /// Per-node soft-tether drag offsets in screen pixels. The user
    /// grabs the panel header and drags; the per-frame delta is
    /// accumulated here. Cleared per-node on a "re-snap" click. The
    /// hover preview shares the map with the promoted panel, keyed by
    /// node idx, so promoting a hovered-and-dragged panel preserves
    /// the user's offset.
    anchored_drag_offsets: HashMap<u32, egui::Vec2>,
    /// NodeMeta for the currently-promoted anchored panel. Filled by
    /// `promoted_anchored_fetch`. Cleared when `promoted_anchored_idx`
    /// changes to a different id.
    promoted_anchored_meta: Option<proto::NodeMeta>,
    /// Async slot for the in-flight /node/:id fetch backing the
    /// promoted anchored panel. Distinct from `hover_preview_fetch`
    /// because the promoted panel survives a hover end and may need
    /// its own fetch lifecycle when the user clicks before the hover
    /// preview's fetch completes.
    promoted_anchored_fetch: Arc<Mutex<Option<Result<Option<proto::NodeMeta>, String>>>>,

    /// Per-node editor state for the inline page viewer. Keyed by
    /// `meta.id`. Switching between obsidian pages preserves each
    /// page's in-progress edits / mode / save state. Never persisted —
    /// it's an ephemeral edit buffer.
    page_viewer_states: HashMap<String, ui::page_viewer::PageViewerState>,
    /// CommonMark layout cache for the page-viewer's Rendered mode.
    /// Separate from `modal.markdown_cache` so the two surfaces don't
    /// invalidate each other's parsed-AST cache when both are open.
    page_viewer_markdown_cache: egui_commonmark::CommonMarkCache,
    /// In-flight `/vault/page` PUT result. Tagged with `(node_id, new_body)`
    /// so the drain logic can update the right `PageViewerState` AND the
    /// cached `promoted_anchored_meta.body` on success.
    save_in_flight: Arc<Mutex<Option<(String, String, Result<(), String>)>>>,

    /// Inverted index of (field, value) -> node-idx buckets. Populated
    /// by a one-shot async `/graph/meta_summary` fetch in `kick_off_bootstrap`.
    field_index: Option<FieldIndex>,
    /// Async slot the meta_summary fetch task writes into.
    field_index_slot: Arc<Mutex<Option<Result<proto::MetaSummary, String>>>>,

    /// Client-derived per-node categorical metric: hash of each node's
    /// **first-sorted tag** (lexicographic byte order). Built once when
    /// `field_index` first lands and used by:
    ///   - `ColorBy::Tag` / `EdgeColorBy::Tag` (metric key `"tag"`)
    ///   - the `community_source == Tag` override (substituted for the
    ///     server's `community` metric when active).
    ///
    /// `None` until the meta_summary fetch lands. Nodes with no tags get
    /// bucket id `0`.
    tag_community_metric: Option<Vec<f32>>,

    /// Per-frame progress / log surface. Populated by both on-thread
    /// callers (layout warmup, palette previews) and async tasks (the
    /// bootstrap fetch) via a clone-able `ProgressSink`. Drained at the
    /// top of each `update` so the footer renders fresh state.
    pub progress: Progress,

    /// Semantic-action input dispatch (jump-io). Owns the binding set;
    /// `update()` polls it once per frame and routes events to the
    /// existing handlers (palette toggle, modal dismiss, fit-camera).
    /// Pre-existing direct `egui::input` reads for camera drag/zoom
    /// stay where they are until the next migration pass — see
    /// `ui::input::AppAction` for the reserved variants.
    input_ctx: InputCtx<AppAction>,

    // -- WASM debounced sessionStorage persistence ---------------------
    //
    // Eframe's auto-save fires roughly every 30s; a reload between
    // firings would otherwise drop everything done since the last
    // tick. We hash AppState each frame, mark dirty on change, and
    // flush at most once per `PERSIST_DEBOUNCE` window. Native is
    // unaffected — the eframe path is still the only writer there.
    /// Last-frame hash of AppState (JSON-bytes hashed via DefaultHasher).
    /// `None` on the first frame so the very first compare marks dirty
    /// and we get one startup-flush to populate sessionStorage.
    #[cfg(target_arch = "wasm32")]
    state_pushed_hash: Option<u64>,
    /// Set whenever the per-frame hash differs from `state_pushed_hash`.
    /// Cleared once the next debounced flush lands in sessionStorage.
    #[cfg(target_arch = "wasm32")]
    state_dirty: bool,
    /// Most recent debounced flush time. `None` means "never flushed";
    /// the next dirty frame will flush immediately.
    #[cfg(target_arch = "wasm32")]
    last_persist_at: Option<web_time::Instant>,

    // -- Snapshot timeline (always-on, native + WASM) -----------------
    //
    // The Instances panel doubles as a live timeline of every UI state
    // change. We reuse the same JSON-hash diff trick as the WASM
    // persistence layer, but cross-platform and on a separate cadence.
    /// JSON-hash of `state` from the previous frame. `None` means
    /// "never sampled" — the very first tick after `App::new` sets it
    /// and does not push a snapshot (the `default` + `restored` entries
    /// were already pushed during `App::new`).
    snapshot_hash: Option<u64>,
    /// Wall-clock instant of the most recent snapshot push. Used to
    /// throttle the auto-snapshotter — bursts of per-frame slider
    /// drags coalesce into one entry per `SNAPSHOT_INTERVAL` window.
    last_snapshot_at: Option<web_time::Instant>,

    // -- Scrubbable simulation timeline (Phase P3) --------------------------
    //
    // A bounded, delta-compressed ring of node-position frames captured from
    // the live sim (the CPU mirror in `GraphPipelines::positions_cpu`). The
    // Timeline section scrubs over this; while paused we push the selected
    // buffered frame to the GPU via `set_positions`. Session-only — never
    // persisted (the ring is large and meaningless across reloads).
    frame_ring: crate::timeline::FrameRing,
    /// Frame counter for the capture stride (`state.timeline.stride`).
    timeline_capture_idx: u64,
    /// Last (depth, stride) we configured the ring with, so a knob change
    /// reconfigures the ring without rebuilding it every frame.
    timeline_prev_knobs: Option<(usize, usize)>,
    /// Last paused index we pushed to the GPU, so we only re-`set_positions`
    /// when the scrub target actually moves (or a seek is flagged dirty).
    timeline_pushed_idx: Option<usize>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Phase D theme: B&W high-contrast with RGBY accents.
        ui::apply_theme(&cc.egui_ctx);

        // Restore persisted UI state (active section, slider values, etc).
        //
        // The `ui::persist` module owns the load/save logic with cfg-gated
        // bodies for wasm vs native:
        //   * Native: reads `eframe::Storage` (platform-dirs JSON blob).
        //   * WASM: reads browser `sessionStorage` — a tab reload preserves
        //     state; a brand-new tab starts fresh. First-load migration
        //     pulls any pre-existing `eframe::Storage` blob over into
        //     sessionStorage so the cutover is non-destructive.
        // The migration shim for the pre-refactor `LayoutState` shape runs
        // inside `persist`, so callers don't need to mind it here.
        // Snapshot the pristine `AppState::default()` as the very first
        // timeline entry BEFORE loading from persistence — this is the
        // explicit "INCLUDING THE DEFAULT STATE" requirement on the
        // Instances timeline. We capture the default by hand (rather
        // than calling `snapshot_now` on a throwaway value) so that
        // when we subsequently overwrite `state` with the persisted
        // blob, the entry rides along on the loaded state's ring.
        let default_state_json =
            serde_json::to_string(&ui::AppState::default()).unwrap_or_default();
        let default_timestamp_ms = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut state: ui::AppState = ui::persist::load_from_eframe(cc.storage);

        // Share-link bootstrap: if the page was opened with a `#s=<hash>` URL
        // fragment, decode it and let it OVERRIDE the persisted/default state.
        // Guarded so a normal load (no fragment) is unaffected. No-op on native.
        if let Some(hash) = ui::share::fragment_from_location() {
            match ui::share::decode(&hash) {
                Ok(shared) => {
                    log::info!("[graph-renderer] restored state from #s= share fragment");
                    state = shared;
                }
                Err(e) => log::warn!("[graph-renderer] #s= share fragment decode failed: {e}"),
            }
        }

        // Seed the in-memory ring with `default` first, then push a
        // `restored` snapshot for the just-loaded state. If load_from_eframe
        // returned the bare default (no prior session), the two entries
        // will have identical state_json — that's fine, the timeline is
        // about user-visible events, and "started fresh" is one.
        state.snapshots.entries.clear();
        state.snapshots.entries.push(ui::state::StateSnapshot {
            timestamp_ms: default_timestamp_ms,
            source: "default".to_string(),
            state_json: default_state_json,
        });
        state.snapshot_now("restored");
        log::info!(
            "[graph-renderer] timeline seeded with {} entries",
            state.snapshots.entries.len()
        );

        // Install the WASM-only beforeunload hook once: it flushes the
        // most-recently-serialized AppState JSON to sessionStorage on
        // tab close / reload, so changes made between the eframe periodic
        // saves aren't lost. No-op on native.
        ui::persist::install_beforeunload_hook();

        // Cap steps_per_call (gpu-force) so an old persisted 8 doesn't
        // burn the GPU; the cooling / energy knobs are left alone so
        // tuned values survive.
        // No-op for fresh state (no settings entry yet).
        // (Done lazily on first apply via the JSON path — kept here as
        //  a TODO marker so we revisit once steps_per_call clamping
        //  becomes a layout-side concern instead of an app-side one.)

        // Phase B: register GraphPipelines into eframe's wgpu callback resources.
        if let Some(wgpu_state) = cc.wgpu_render_state.as_ref() {
            let device = &wgpu_state.device;
            let format = wgpu_state.target_format;
            let pipes = GraphPipelines::new(device, format);
            wgpu_state
                .renderer
                .write()
                .callback_resources
                .insert(pipes);
            log::info!(
                "[graph-renderer] GraphPipelines registered (target_format = {:?})",
                format
            );
        } else {
            log::warn!("[graph-renderer] no wgpu_render_state — graph layer disabled");
        }

        // Phase C: kick off async bootstrap fetch.
        let load: SharedLoad = Arc::new(Mutex::new(LoadState::Pending));
        let base_url = default_base_url();
        let progress = Progress::new();
        let progress_sink = progress.sink();
        kick_off_bootstrap(load.clone(), base_url.clone(), progress_sink);

        // Long-lived poller for server-side progress (vault ingest /
        // watcher reloads / search reindex). Mirrors backend
        // `ProgressEvent`s into the same `Progress` sink the bootstrap
        // fetch uses, so the existing footer UI surfaces ingest task
        // bars without any extra glue.
        kick_off_progress_poll(base_url.clone(), progress.sink());

        // Long-lived compute-broker health watcher. Polls
        // `/compute/health` every 2s and emits a footer-log event on
        // every state transition (connected ↔ disconnected). Without
        // this signal, a stalled Remote FA2 stream reads as a frontend
        // bug — in fact graph-api is up but its gRPC dial to
        // graph-compute is failing.
        let server_reachable: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
        {
            let sink = progress.sink();
            let client = ApiClient::new(base_url.clone());
            let reachable = server_reachable.clone();
            spawn_async(async move {
                use std::time::Duration;
                let mut last_known: Option<bool> = None;
                loop {
                    match client.compute_health().await {
                        Ok(h) => {
                            // graph-api answered → reachable for /generate.
                            *reachable.lock().unwrap() = true;
                            if last_known != Some(h.connected) {
                                last_known = Some(h.connected);
                                if h.connected {
                                    sink.info(
                                        "compute",
                                        format!("broker connected to {}", h.url),
                                    );
                                } else {
                                    sink.warn(
                                        "compute",
                                        format!(
                                            "broker disconnected from {}",
                                            if h.url.is_empty() { "(no url set)" } else { &h.url }
                                        ),
                                    );
                                }
                            }
                        }
                        Err(_) => {
                            // graph-api itself is down or the route 404s →
                            // /generate is not reachable; Auto falls back local.
                            *reachable.lock().unwrap() = false;
                            // Don't spam the log — only emit once on
                            // transition.
                            if last_known != Some(false) {
                                last_known = Some(false);
                                sink.warn("compute", "broker status unreachable");
                            }
                        }
                    }
                    sleep_async(Duration::from_secs(2)).await;
                }
            });
        }

        // One-shot meta_summary fetch in parallel with the bootstrap.
        // Used to power active-filter chips + SharedTag/Filter focus.
        let field_index_slot: Arc<Mutex<Option<Result<proto::MetaSummary, String>>>> =
            Arc::new(Mutex::new(None));
        {
            let slot = field_index_slot.clone();
            let client = ApiClient::new(base_url.clone());
            // Surface the fetch in the status footer so the user can tell
            // metadata is still loading rather than guess from a blank
            // sidebar.
            let sink = progress.sink();
            spawn_async(async move {
                let task = sink.start("meta", "fetching field index");
                let res = client.meta_summary().await;
                match &res {
                    Ok(_) => sink.finish(task),
                    Err(e) => sink.fail(task, e.clone()),
                }
                *slot.lock().unwrap() = Some(res);
            });
        }

        let mut actions = ActionRegistry::new();
        actions::seed_default_actions(&mut actions);
        // Rehydrate persisted ActionInstances. The registry is re-seeded
        // each startup; only the live instance list survives.
        actions.instances = state.action_instances.clone();
        actions.next_instance_id = actions
            .instances
            .iter()
            .map(|i| i.id)
            .max()
            .unwrap_or(0);

        Self {
            state,
            load,
            loaded_into_gpu: false,
            pending_fit_on_load: false,
            logged_ready: false,
            modal: ui::ModalState::default(),
            node_fetch: Arc::new(Mutex::new(None)),
            generate_job: None,
            ids: Vec::new(),
            id_to_idx: HashMap::new(),
            metrics: HashMap::new(),
            base_url,
            server_reachable,
            search_cache: Arc::new(Mutex::new(HashMap::new())),
            search_inflight: HashSet::new(),
            prev_style_key: None,
            prev_layout_key: None,
            prev_seed_mode: None,
            prev_focus_key: None,
            prev_cursor_key: None,
            prev_selected_hash: None,
            prev_canvas_rect: None,
            last_pointer_in_canvas: None,
            cursor_force_active: 0.0,
            prev_cursor_force_active: 0.0,
            post_click_cooldown_frames: 0,
            post_click_cooldown_applied: false,
            last_observed_max_ke: 0.0,
            palette_state: ui::CommandPaletteState::default(),
            actions,
            palette_preview_slot: Arc::new(Mutex::new(None)),
            palette_inflight: HashSet::new(),
            layout_registry: LayoutRegistry::seed_default(),
            prev_active_layout_id: None,
            last_fit_screen: None,
            camera_pan_accel_t: 0.0,
            perf: PerfCollector::default(),
            selected_node_idx: None,
            focus_sticky_idx: None,
            focus_hover_idx: None,
            focus_hover_edge_idx: None,
            focus_pushed_idx: None,
            focus_pushed_mode: None,
            filter_pushed_sig: None,
            hovered_pushed_idx: None,
            hovered_pushed_edge_idx: None,
            previous_focus_mode: None,
            prev_filters_non_empty: false,
            last_hover_raycast_at: None,
            hover_clear_at: None,
            hover_preview_idx: None,
            hover_preview_armed_at: None,
            hover_preview_meta: None,
            hover_preview_fetch: Arc::new(Mutex::new(None)),
            hover_preview_open: false,
            hover_preview_pos: None,
            last_anchored_screen_pos: HashMap::new(),
            promoted_anchored_idx: None,
            node_panel_collapsed: HashMap::new(),
            anchored_drag_offsets: HashMap::new(),
            promoted_anchored_meta: None,
            promoted_anchored_fetch: Arc::new(Mutex::new(None)),
            page_viewer_states: HashMap::new(),
            page_viewer_markdown_cache: egui_commonmark::CommonMarkCache::default(),
            save_in_flight: Arc::new(Mutex::new(None)),
            field_index: None,
            field_index_slot,
            tag_community_metric: None,
            progress,
            input_ctx: InputCtx::new(default_bindings()),

            #[cfg(target_arch = "wasm32")]
            state_pushed_hash: None,
            #[cfg(target_arch = "wasm32")]
            state_dirty: false,
            #[cfg(target_arch = "wasm32")]
            last_persist_at: None,

            snapshot_hash: None,
            last_snapshot_at: None,

            // Ring built at the persisted/default depth + a 1-keyframe-every-8
            // cadence (delta-compress 7 of every 8 frames). `tick_timeline`
            // reconciles the depth/stride knobs each frame.
            frame_ring: crate::timeline::FrameRing::new(300, 8),
            timeline_capture_idx: 0,
            timeline_prev_knobs: None,
            timeline_pushed_idx: None,
        }
    }

    /// Drain any completed palette preview fetches and kick off new ones
    /// requested by the palette during the previous frame.
    fn service_palette_preview(&mut self) {
        let drained = self.palette_preview_slot.lock().unwrap().take();
        if let Some((id, result)) = drained {
            self.palette_inflight.remove(&id);
            match result {
                Ok(meta) => {
                    self.palette_state.preview_cache.insert(id, meta);
                }
                Err(e) => {
                    self.palette_state.preview_errors.insert(id, e);
                }
            }
        }
        if let Some(id) = self.palette_state.pending_preview_id.take() {
            if !self.palette_state.preview_cache.contains_key(&id)
                && !self.palette_state.preview_errors.contains_key(&id)
                && self.palette_inflight.insert(id.clone())
            {
                let slot = self.palette_preview_slot.clone();
                let client = ApiClient::new(self.base_url.clone());
                let id_for_task = id.clone();
                let sink = self.progress.sink();
                let label = format!("preview {}", short_id(&id_for_task));
                spawn_async(async move {
                    let task = sink.start("palette", label);
                    // `client.node(...)` returns Ok(None) on a 404. The
                    // palette preview surface treats "not found" as an
                    // error message rather than carrying a tri-state into
                    // the cache; map None -> Err("not found") here.
                    let result = match client.node(&id_for_task).await {
                        Ok(Some(meta)) => Ok(meta),
                        Ok(None) => Err("not found".to_string()),
                        Err(e) => Err(e),
                    };
                    match &result {
                        Ok(_) => sink.finish(task),
                        Err(e) => sink.fail(task, e.clone()),
                    }
                    *slot.lock().unwrap() = Some((id_for_task, result));
                });
            }
        }
    }

    /// Spawn a `/node/:id` fetch dedicated to the promoted anchored
    /// panel. Distinct slot so the modal-side `node_fetch` drain logic
    /// keeps owning that slot and we don't fight it for ownership.
    fn kick_off_promoted_anchored_fetch(&self, id: String) {
        let slot = self.promoted_anchored_fetch.clone();
        let client = ApiClient::new(self.base_url.clone());
        let sink = self.progress.sink();
        let label = format!("promote {}", short_id(&id));
        spawn_async(async move {
            let task = sink.start("anchored", label);
            let res = client.node(&id).await;
            match &res {
                Ok(_) => sink.finish(task),
                Err(e) => sink.fail(task, e.clone()),
            }
            *slot.lock().unwrap() = Some(res);
        });
    }

    /// Spawn an async `PUT /vault/page` save. Result lands in
    /// `save_in_flight`, tagged with `(node_id, body)` so the drain
    /// step can route the outcome back to the right `PageViewerState`.
    fn kick_off_page_save(&self, node_id: String, path: String, body: String) {
        let slot = self.save_in_flight.clone();
        let client = ApiClient::new(self.base_url.clone());
        let sink = self.progress.sink();
        let label = format!("save {}", short_id(&path));
        spawn_async(async move {
            let task = sink.start("save", label);
            let res = client.save_page(&path, &body).await;
            match &res {
                Ok(_) => sink.finish(task),
                Err(e) => sink.fail(task, e.clone()),
            }
            *slot.lock().unwrap() = Some((node_id, body, res));
        });
    }

    /// Spawn an async `/node/:id` fetch. The result lands in `self.node_fetch`
    /// and gets drained into the modal on the next frame's `update`.
    fn kick_off_node_fetch(&self, id: String) {
        let slot = self.node_fetch.clone();
        let client = ApiClient::new(self.base_url.clone());
        let sink = self.progress.sink();
        let label = format!("fetch node {}", short_id(&id));
        let id_for_task = id.clone();
        spawn_async(async move {
            let task = sink.start("node", label);
            let result = client.node(&id_for_task).await;
            match &result {
                Ok(_) => sink.finish(task),
                Err(e) => sink.fail(task, e.clone()),
            }
            *slot.lock().unwrap() = Some(result);
        });
    }

    /// Focus a node by id: slide the camera onto its world position, mark
    /// it as the sticky-focused node (so community-dim/effects highlight
    /// it), and refresh the modal/sidebar to show its details. No-op if
    /// the id isn't in the loaded graph.
    ///
    /// Called from the badge → focus-node flow: every interactive badge
    /// that knows what node it belongs to routes its body-click through
    /// here so the user gets camera+sidebar+highlight in one motion.
    fn focus_node_by_id(&mut self, frame: &mut eframe::Frame, id: &str) {
        // Refresh the modal regardless of whether the camera move succeeds
        // — the user expects clicking a badge to update the sidebar even
        // if positions haven't streamed back yet.
        self.kick_off_node_fetch(id.to_string());

        let Some(&idx) = self.id_to_idx.get(id) else {
            return;
        };
        self.focus_sticky_idx = Some(idx);

        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };
        let mut renderer = wgpu_state.renderer.write();
        let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() else {
            return;
        };
        let Some(pos) = pipes.position_of(idx) else {
            return;
        };
        // Distance scales with graph radius so a small graph doesn't fly
        // the camera way back and a huge graph still lets the node fill
        // ~25% of the viewport. Falls back to a fixed value if bounds
        // haven't been computed (eg. boot before first positions stream).
        let distance = pipes
            .bounds()
            .map(|(mn, mx)| ((mx - mn) * 0.5).length().max(50.0) * 0.6)
            .unwrap_or(500.0);
        pipes.camera.look_at_point(pos, distance);
    }

    /// Drain a completed `/graph/meta_summary` fetch and decode it into
    /// the local FieldIndex, if a result has landed.
    fn drain_field_index(&mut self) {
        if self.field_index.is_some() {
            return;
        }
        let result_opt = self.field_index_slot.lock().unwrap().take();
        let Some(result) = result_opt else { return };
        match result {
            Ok(meta) => {
                let fi = FieldIndex::from_proto(&meta, &self.ids);
                log::info!(
                    "[graph-renderer] meta_summary: {} fields, {} buckets",
                    fi.by_field.len(),
                    fi.by_field.values().map(|m| m.len()).sum::<usize>(),
                );
                // Precompute the tag-derived per-node metric (used by
                // ColorBy::Tag, EdgeColorBy::Tag, and the
                // `community_source == Tag` override). Re-derived once
                // here so the per-frame `apply_style_to_gpu` doesn't pay
                // the cost on every recompute.
                self.tag_community_metric = fi.tag_primary_metric(self.ids.len());
                self.field_index = Some(fi);
                // Force a style recompute so the new metric flows into
                // the GPU buffers without waiting for the user to wiggle
                // a slider.
                self.prev_style_key = None;
            }
            Err(e) => {
                log::warn!("[graph-renderer] meta_summary fetch failed: {e}");
            }
        }
    }

    /// Build a per-style view of `self.metrics` that honours both the
    /// `ColorBy::Tag` / `EdgeColorBy::Tag` metric key AND the
    /// `community_source == Tag` override. When neither path needs the
    /// tag-derived metric, the original `&self.metrics` is reused with
    /// zero clones (Cow::Borrowed).
    ///
    /// Tag tiebreaker: each node's primary tag is the **first tag in
    /// lexicographic byte order** drawn from its `tags` set. Untagged
    /// nodes get bucket id `0`. See
    /// `crate::ui::field_index::FieldIndex::tag_primary_metric`.
    fn metrics_view(&self) -> std::borrow::Cow<'_, HashMap<String, Vec<f32>>> {
        let Some(tag_m) = self.tag_community_metric.as_ref() else {
            return std::borrow::Cow::Borrowed(&self.metrics);
        };
        let override_community =
            self.state.style.community_source == crate::ui::state::CommunitySource::Tag;
        // Skip the clone if no consumer needs the tag metric. The
        // categorical-key list mirrors `colors_from_metric` /
        // `edge_colors_from_metric` / `shapes_from_metric`.
        let needs_tag_key = self.state.style.color_by == ColorBy::Tag
            || self.state.style.edge_color_by == EdgeColorBy::Tag;
        if !override_community && !needs_tag_key {
            return std::borrow::Cow::Borrowed(&self.metrics);
        }
        let mut m = self.metrics.clone();
        m.insert("tag".to_string(), tag_m.clone());
        if override_community {
            m.insert("community".to_string(), tag_m.clone());
        }
        std::borrow::Cow::Owned(m)
    }

    /// Drain a completed `/node/:id` fetch into the modal state, if any.
    ///
    /// `Ok(Some)` opens the modal as before. `Ok(None)` (server 404) is a
    /// soft outcome — log at debug level only and leave the modal closed,
    /// so we don't spam the console for ids that legitimately aren't in
    /// the in-memory graph (these largely live in the Prisma DB now).
    /// `Err(e)` is a real transport/decode failure and still surfaces.
    fn drain_node_fetch(&mut self) {
        let result_opt = self.node_fetch.lock().unwrap().take();
        let Some(result) = result_opt else { return };
        match result {
            Ok(Some(meta)) => {
                log::info!("[graph-renderer] modal: fetched node {}", meta.id);
                self.modal.fetch_error = None;
                self.modal.current = Some(meta);
                // The free-standing modal window is no longer auto-opened
                // — the anchored panel (compact on hover, expandable to
                // the full inspector body via the header toggle) is the
                // unified node-view surface. `modal.current` is still
                // populated for InspectorData.current_meta consumers, and
                // a future cmd+P / explicit-open path can flip
                // `modal.open` if a separate full window is ever wanted.
            }
            Ok(None) => {
                log::debug!("[graph-renderer] modal: node not found (404), no modal");
            }
            Err(e) => {
                log::warn!("[graph-renderer] modal: fetch error: {e}");
                self.modal.fetch_error = Some(e);
                self.modal.open = true;
            }
        }
    }

    /// Look up the string id for a node index from the cached bootstrap ids.
    fn id_for_idx(&self, idx: u32) -> Option<String> {
        self.ids.get(idx as usize).cloned()
    }

    /// Run a raycast against GraphPipelines and return the hit node index.
    /// `screen_px` is the click-frame canvas rect width/height — passed in
    /// so picking uses the *current* aspect even though the GraphCallback's
    /// `prepare()` (which would update `pipes.screen_px`) doesn't run until
    /// after `App::update` returns.
    fn raycast_idx(
        &self,
        frame: &eframe::Frame,
        ndc_x: f32,
        ndc_y: f32,
        screen_px: [f32; 2],
    ) -> Option<u32> {
        let wgpu_state = frame.wgpu_render_state()?;
        let renderer = wgpu_state.renderer.read();
        let pipes = renderer.callback_resources.get::<GraphPipelines>()?;
        pipes.raycast(ndc_x, ndc_y, screen_px)
    }

    /// Drive the Generate (tvix) evaluation job: kick one off when the panel
    /// has recorded a `request`, then poll an in-flight job and route its
    /// result back through the existing `state.generate.pending` handoff.
    ///
    /// This is what keeps a large `eval_graph` off the click-handler. The
    /// work runs via [`crate::job::BackgroundJob`] — genuinely off-thread on
    /// native, paint-first-then-run on WASM (see the job module docs) — with
    /// coarse progress (`queued → evaluating → done + counts`) surfacing in
    /// the footer task list and the debug console.
    /// Resolve the concrete [`crate::job::ExecutionBackend`] for the next
    /// generate, honouring the panel's `GenerateBackendChoice` and the `Auto`
    /// default. `Auto` picks **Server when graph-api is reachable**, else a
    /// local fallback (LocalWorker on wasm — a real Web Worker, see
    /// [`crate::worker`] — Inline on native). This mirrors the local-vs-remote
    /// layout-engine default.
    fn resolve_generate_backend(&self) -> crate::job::ExecutionBackend {
        let reachable = *self.server_reachable.lock().unwrap();
        resolve_generate_backend(
            self.state.generate.backend,
            reachable,
            cfg!(target_arch = "wasm32"),
        )
    }

    /// Spawn a Generate eval on the LOCAL executor (native thread / wasm
    /// paint-first-then-run). Shared by the `Inline` backend and the native
    /// `LocalWorker` fallback (native has no Web Worker; Inline already runs on
    /// a real thread).
    fn spawn_local_eval(
        &self,
        src: String,
    ) -> crate::job::BackgroundJob<tvix_wasm::GeneratedGraph> {
        crate::job::BackgroundJob::spawn(
            self.progress.sink(),
            "generate",
            "evaluate expression (local)",
            move |s| {
                s.info("generate", "evaluating Nix expression…");
                let graph = tvix_wasm::eval_graph(&src)?;
                s.info(
                    "generate",
                    format!(
                        "evaluated: {} nodes, {} edges — building graph",
                        graph.nodes.len(),
                        graph.edges.len()
                    ),
                );
                Ok(graph)
            },
        )
    }

    fn pump_generate_job(&mut self) {
        // 1. Kick off a job from a pending request (one job at a time — the
        //    panel guards against re-requesting while `request` is set, and we
        //    only spawn when no job is in flight). The chosen ExecutionBackend
        //    routes WHERE the work runs; the spawn/poll interface + progress UI
        //    are identical across backends.
        if self.generate_job.is_none() {
            if let Some(src) = self.state.generate.request.take() {
                let backend = self.resolve_generate_backend();
                let job = match backend {
                    crate::job::ExecutionBackend::Server => {
                        // PRIMARY non-freeze: eval runs server-side; the client
                        // call is async, so the egui thread never blocks.
                        let client = ApiClient::new(self.base_url.clone());
                        crate::job::BackgroundJob::spawn_future(
                            self.progress.sink(),
                            "generate",
                            "evaluate expression (server)",
                            async move { client.generate_remote(&src).await },
                        )
                    }
                    // Inline: run the closure on the local executor — native
                    // thread (genuinely off-thread) / wasm paint-first-then-run
                    // (the single synchronous eval still blocks its frame).
                    crate::job::ExecutionBackend::Inline => self.spawn_local_eval(src),
                    // LocalWorker: on wasm, evaluate inside the `tvix-worker`
                    // Web Worker — a second wasm instance in a browser-owned
                    // thread, so the egui thread never blocks (the OFFLINE
                    // non-freeze, no graph-api needed). On native there is no
                    // Web Worker and Inline already runs on a real thread, so
                    // it falls back to the same local executor.
                    crate::job::ExecutionBackend::LocalWorker => {
                        #[cfg(target_arch = "wasm32")]
                        {
                            crate::job::BackgroundJob::spawn_future(
                                self.progress.sink(),
                                "generate",
                                "evaluate expression (worker)",
                                crate::worker::eval_in_worker(src),
                            )
                        }
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            self.spawn_local_eval(src)
                        }
                    }
                };
                self.generate_job = Some(job);
            }
        }

        // 2. Poll an in-flight job. On WASM the first poll runs the (deferred)
        //    work — egui painted the "queued" frame between spawn and now.
        if let Some(job) = self.generate_job.as_mut() {
            if let Some(result) = job.poll() {
                self.generate_job = None;
                match result {
                    Ok(graph) => {
                        self.state.generate.editor.error = None;
                        self.state.generate.editor.status = Some(format!(
                            "{} nodes, {} edges",
                            graph.nodes.len(),
                            graph.edges.len()
                        ));
                        // Hand to the existing promotion path below.
                        self.state.generate.pending = Some(graph);
                    }
                    Err(err) => {
                        // Surface the eval error inline in the panel, exactly
                        // as the old inline path did.
                        self.state.generate.editor.status = None;
                        self.state.generate.editor.error = Some(err);
                    }
                }
            } else {
                // Still running — keep the frame loop alive so we re-poll
                // (and on WASM actually drive the deferred work) next frame.
                // The caller (`update`) requests a repaint when a job is live.
            }
        }
    }

    /// `true` while a Generate evaluation job is in flight — used by `update`
    /// to keep requesting repaints so the job keeps getting polled.
    fn generate_job_active(&self) -> bool {
        self.generate_job.is_some() || self.state.generate.request.is_some()
    }

    /// Take a pending graph produced by the Generate (tvix) eval job, convert
    /// it to a [`Bootstrap`], publish it as `LoadState::Ready`, and reset the
    /// GPU load latch so `try_promote_bootstrap_to_gpu` re-loads it this
    /// frame — replacing the live graph. The panel itself has no access to
    /// `self.load` / GPU pipelines, so it hands off through the one-shot
    /// `state.generate.pending` field (set by `pump_generate_job` when the
    /// background eval completes).
    fn drain_generated_graph(&mut self) {
        let Some(graph) = self.state.generate.pending.take() else {
            return;
        };
        let mut bootstrap = crate::generate::bootstrap_from_generated(&graph);
        // Honour the Initial-seed strategy for the generated graph's INITIAL
        // positions, instead of always imposing the default sphere shell. With
        // "No seed" selected this is a minimal jitter (the force sim builds the
        // layout from there) — so "No seed" actually means no pre-arranged seed,
        // even on generation. A chosen built-in/custom seed places the new nodes
        // accordingly.
        let n = bootstrap.positions.len() / 3;
        bootstrap.positions = crate::generate::seed_positions_for(
            &self.state.seed.strategy,
            &self.state.seed.editor.source,
            n,
        );
        *self.load.lock().unwrap() = LoadState::Ready(bootstrap);
        // Re-arm the one-shot upload latch so the new bootstrap is promoted
        // (GraphPipelines::load reallocates all buffers fresh each call).
        self.loaded_into_gpu = false;
        // Force a fresh ready-log line for the regenerated graph.
        self.logged_ready = false;
        // Frame the regenerated graph once it lands on the GPU.
        self.pending_fit_on_load = true;
        // Drop all node-idx-keyed session state: the replacement graph
        // renumbers (or removes) every index, so any carried-over
        // selection / focus / hover / picking / GPU-write-gate would now
        // point at the wrong node (or out of bounds).
        self.reset_session_state_for_new_graph();
    }

    /// Clear every piece of per-graph session state so a freshly loaded
    /// graph starts clean. Node indices are only meaningful relative to a
    /// single loaded graph; replacing the graph invalidates every cached
    /// `u32` idx (selection, focus, hover, anchored panels) AND every
    /// "already pushed to GPU" change-detection tracker (so the new
    /// buffers get re-seeded with style / colors / focus on the next
    /// frame instead of being skipped as unchanged). Mirrors the relevant
    /// `App::new` defaults; does NOT touch persisted `AppState` or async
    /// fetch slots tied to id strings that survive a graph swap.
    fn reset_session_state_for_new_graph(&mut self) {
        // Selection / inspector.
        self.selected_node_idx = None;
        // Focus mode (sticky + hover + edge hover).
        self.focus_sticky_idx = None;
        self.focus_hover_idx = None;
        self.focus_hover_edge_idx = None;
        self.hover_clear_at = None;
        self.last_hover_raycast_at = None;
        // Hover-preview card.
        self.hover_preview_idx = None;
        self.hover_preview_armed_at = None;
        self.hover_preview_meta = None;
        self.hover_preview_open = false;
        self.hover_preview_pos = None;
        // Anchored / promoted panels (all idx-keyed).
        self.promoted_anchored_idx = None;
        self.promoted_anchored_meta = None;
        self.last_anchored_screen_pos.clear();
        self.node_panel_collapsed.clear();
        self.anchored_drag_offsets.clear();
        // GPU-write change-detection gates: force a fresh push into the
        // newly allocated buffers (otherwise an unchanged key skips the
        // re-upload and the new graph renders against stale GPU state).
        self.prev_style_key = None;
        self.prev_layout_key = None;
        self.prev_seed_mode = None;
        self.prev_focus_key = None;
        self.prev_cursor_key = None;
        self.prev_selected_hash = None;
        self.prev_active_layout_id = None;
        self.focus_pushed_idx = None;
        self.focus_pushed_mode = None;
        self.filter_pushed_sig = None;
        self.hovered_pushed_idx = None;
        self.hovered_pushed_edge_idx = None;
        // Cursor-force / post-click cooldown bookkeeping.
        self.cursor_force_active = 0.0;
        self.prev_cursor_force_active = 0.0;
        self.post_click_cooldown_frames = 0;
        self.post_click_cooldown_applied = false;
        self.last_observed_max_ke = 0.0;
        // Tag-derived metric is rebuilt from the field index, which is
        // server-backed and absent for tvix-generated graphs; clear it so
        // a stale (wrong-length) tag metric can't reach colors_from_metric.
        self.tag_community_metric = None;
        self.field_index = None;
    }

    fn try_promote_bootstrap_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if self.loaded_into_gpu {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };

        let bootstrap_opt: Option<Bootstrap> = {
            let mut guard = self.load.lock().unwrap();
            match std::mem::take(&mut *guard) {
                LoadState::Ready(b) => Some(b),
                other => {
                    *guard = other;
                    None
                }
            }
        };
        let Some(mut bootstrap) = bootstrap_opt else {
            return;
        };

        // Cache the idx -> id table for click-to-modal resolution before we
        // consume the rest of the bootstrap into GPU buffers.
        self.ids = bootstrap.ids.clone();
        self.id_to_idx = bootstrap
            .ids
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i as u32))
            .collect();
        self.metrics = bootstrap.metrics.clone();

        let n_nodes = bootstrap.positions.len() / 3;

        // Multilevel coarsening warm-up (FM3 / sfdp). Replace the server's
        // random initial layout with a coarsened-cascade seed so the GPU
        // sim converges in a handful of frames instead of hundreds. No-op
        // for n_nodes < 64 (handled inside warmup_positions).
        // Pull spring_len from the active gpu-force settings JSON, falling
        // back to defaults if absent (fresh install, non-gpu-force active).
        let spring_len = self
            .state
            .layout
            .settings
            .get("gpu-force")
            .and_then(|v| v.get("spring_len"))
            .and_then(|v| v.as_f64())
            .map(|f| f as f32)
            .unwrap_or_else(|| GpuForceOptions::default().spring_len)
            .max(1.0);
        let warmed = {
            let _scope = self.progress.scope(
                "layout",
                format!("multilevel coarsening ({n_nodes} nodes)"),
            );
            warmup_positions(n_nodes, &bootstrap.edges, spring_len, 0xC0A75E)
        };
        if warmed.len() == bootstrap.positions.len() {
            bootstrap.positions = warmed;
            log::info!(
                "[graph-renderer] coarsening warmup applied ({} nodes)",
                n_nodes
            );
            self.progress.info(
                "layout",
                format!("coarsening warmup applied ({n_nodes} nodes)"),
            );
        }

        // Initial colors / sizes from the user's persisted style choice.
        // `metrics_view` substitutes the tag-derived metric for the
        // `"tag"` key and (when `community_source == Tag`) for `"community"`.
        let mv = self.metrics_view();
        let colors = data::colors_from_metric(
            self.state.style.color_by.metric_key(),
            mv.as_ref(),
            n_nodes,
            self.state.style.palette,
        );
        let sizes = data::sizes_from_metric(
            self.state.style.size_by.metric_key(),
            &self.metrics,
            n_nodes,
            apply_size_scale(self.state.style.size_mul, self.state.style.log_scale_size),
        );
        drop(mv);
        let graph = GraphData {
            positions: bootstrap.positions,
            edges: bootstrap.edges,
            colors,
            sizes,
        };

        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let upload = self.progress.scope("layout", "first GPU step pending");
        let mut load_result: Result<Option<(u32, u32)>, String> = Ok(None);
        {
            let mut renderer = wgpu_state.renderer.write();
            if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
                match pipes.load(&device, &queue, graph) {
                    Ok(()) => {
                        log::info!(
                            "[graph-renderer] graph loaded: {} nodes, {} edges",
                            pipes.n_nodes(),
                            pipes.n_edges()
                        );
                        // Frame a regenerated graph in view (its bounds
                        // differ from the previous graph's; the idle
                        // auto-refit only fires on a window resize).
                        if self.pending_fit_on_load {
                            pipes.fit_camera();
                        }
                        load_result = Ok(Some((pipes.n_nodes(), pipes.n_edges())));
                    }
                    Err(e) => {
                        log::error!("[graph-renderer] GraphPipelines::load failed: {e}");
                        load_result = Err(format!("{e}"));
                    }
                }
            }
        }
        match load_result {
            Ok(Some((n_nodes_g, n_edges_g))) => {
                drop(upload);
                self.progress.info(
                    "layout",
                    format!("GPU buffers ready: {n_nodes_g} nodes, {n_edges_g} edges"),
                );
                self.loaded_into_gpu = true;
                // The fit (if requested) already ran while we held the
                // pipes write lock above. Disarm the one-shot and reset the
                // auto-refit baseline to `None` — the idle handler treats
                // `None` as "initial fit already done, skip" (it only
                // refits on a measured screen-size change), so we won't
                // double-fit on the next frame.
                if self.pending_fit_on_load {
                    self.last_fit_screen = None;
                    self.pending_fit_on_load = false;
                }
            }
            Ok(None) => {
                drop(upload);
            }
            Err(e) => {
                upload.fail(e);
            }
        }
    }

    fn emit_ready_log(&mut self, frame: &mut eframe::Frame) {
        if self.logged_ready || !self.loaded_into_gpu {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };
        let renderer = wgpu_state.renderer.read();
        let Some(pipes) = renderer.callback_resources.get::<GraphPipelines>() else {
            return;
        };
        log::info!(
            "[graph-renderer] graph loaded: {} nodes",
            pipes.n_nodes()
        );
        #[cfg(target_arch = "wasm32")]
        {
            web_sys::console::log_1(
                &format!(
                    "[graph-renderer] graph loaded: {} nodes",
                    pipes.n_nodes()
                )
                .into(),
            );
        }
        self.logged_ready = true;
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Cosmograph #050710 — deep cool near-black. Alpha-stacked edges
        // read against this instead of mid-grey. The sidebar UI alone
        // clears the test harness's brightFrac > 0.01 threshold.
        [0.020, 0.027, 0.063, 1.0]
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // On WASM the source of truth is sessionStorage; we still mirror
        // to `eframe::Storage` so test harnesses that introspect that key
        // keep working. On native, this writes the platform-dirs blob.
        ui::persist::save_to_eframe(storage, &self.state);
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.perf.begin_frame();
        // Re-apply theme each frame so hot edits to theme.rs land without restart.
        ui::apply_theme(ctx);

        // Drain progress events posted by async tasks before any UI runs
        // so the footer renders fresh state this frame.
        self.progress.drain_sink();

        // Pump the Generate (tvix) eval job: spawn one from a pending panel
        // request, poll an in-flight one, and route its result into
        // `state.generate.pending`. While a job is live we keep requesting
        // repaints so it stays polled (on WASM the first poll runs the
        // deferred work after a paint-first frame).
        self.pump_generate_job();
        if self.generate_job_active() {
            ctx.request_repaint();
        }

        // Drain a freshly generated (tvix) graph, if any, into the shared
        // load slot so the promote path below replaces the live graph.
        self.drain_generated_graph();

        // Pump the data pipeline.
        self.try_promote_bootstrap_to_gpu(frame);
        self.emit_ready_log(frame);

        // Drain any completed /node/:id fetch into the modal.
        self.drain_node_fetch();
        self.drain_field_index();

        // Drain semantic input events for this frame. Pulses (Cmd+P,
        // F, Esc) are consumed here; axis events (WASDQE pan, RMB/MMB
        // rotate, wheel/pinch zoom) get partitioned into a per-frame
        // Vec passed through `WorkspaceCtx` to ui::workspace.
        //
        // Esc is consumed below near the modal-close site so the
        // palette's own internal Esc handler still wins.
        let dt = ctx.input(|i| i.stable_dt);
        let raw = ctx.input(|i| egui_raw(i, dt));
        let want_kbd = ctx.wants_keyboard_input();
        let mut want_open_palette = false;
        let mut want_fit = false;
        let mut want_cancel = false;
        let mut camera_input_events: Vec<InputEvent<AppAction>> = Vec::new();
        for ev in self.input_ctx.poll(&raw) {
            match &ev {
                InputEvent::Pulse(AppAction::OpenPalette) => want_open_palette = true,
                // F-to-fit is suppressed while a text edit owns the
                // keyboard so the user can type "F" into the palette
                // search box without flying the camera.
                InputEvent::Pulse(AppAction::FitCamera) if !want_kbd => want_fit = true,
                InputEvent::Pulse(AppAction::FitCamera) => {}
                InputEvent::Pulse(AppAction::Cancel) => want_cancel = true,
                // Camera-axis events go to workspace.
                _ => camera_input_events.push(ev),
            }
        }
        // Stash for the WorkspaceCtx. WorkspaceCtx borrows the slice;
        // we keep ownership on the stack here for the lifetime of
        // this `update` call.
        let camera_input_events = camera_input_events;
        if want_open_palette {
            self.palette_state.toggle();
        }
        if want_fit {
            self.execute_action(frame, "fit-camera", HashMap::new());
        }

        // Phase D sidebar (activity bar + section panel) on the left.
        self.perf.begin_stage(StageId::UiChrome);
        ui::sidebar::show_floating(
            ctx,
            &mut self.state,
            &mut self.actions,
            &self.layout_registry,
            &self.perf,
        );

        // The inspector body is no longer rendered as a standalone
        // panel — it now lives inside the unified anchored panel
        // (`render_anchored_panel`, called from `show_hover_preview`
        // near the end of `update`). The channel-drain logic that
        // used to live here moved to *after* `show_hover_preview`
        // — see "post-anchored-panel inspector channel drain" below.

        // Drain any palette preview-fetch result from the previous frame
        // and kick off any new request the palette flagged. Done before
        // re-rendering so a freshly-arrived NodeMeta paints the same frame.
        self.service_palette_preview();

        // Command palette modal — runs above the sidebar, below the modal.
        let palette_outcome = ui::show_command_palette(
            ctx,
            &mut self.palette_state,
            &mut self.actions,
            &self.state.workspace,
            &self.ids,
        );
        self.perf.end_stage(StageId::UiChrome);
        match palette_outcome {
            PaletteOutcome::Execute { action_id, params } => {
                // Attribute the next auto-snapshot to the palette action.
                // Prefer the action's human-readable `title` over its id;
                // the id is e.g. `"focus.fit"` while the title is
                // `"Focus: fit"` which reads better in the timeline.
                let label = self
                    .actions
                    .get(&action_id)
                    .map(|a| a.title.clone())
                    .unwrap_or_else(|| action_id.clone());
                self.state.snapshot_source = Some(format!("palette: {label}"));
                self.state.frontend_events.push("palette", label.clone());
                self.execute_action(frame, &action_id, params);
            }
            PaletteOutcome::OpenNode { id } => {
                self.kick_off_node_fetch(id);
            }
            PaletteOutcome::None => {}
        }

        // Bottom-of-screen panel stack: egui stacks bottom panels in
        // registration order with the FIRST being outermost. So:
        //   1. Tray strip (always-visible icon launcher, fixed height).
        //   2. Status footer (expand/contract sticky panel, sits ABOVE
        //      the tray, shows task list + log buffer when expanded).
        // The user wants the tray pinned to the absolute bottom while
        // the status footer collapses/expands without ever covering the
        // tray. Register tray first.
        ui::status_footer::show_tray(ctx, &mut self.state, &self.progress);
        ui::status_footer::show(
            ctx,
            &mut self.state.status_footer_open,
            &mut self.progress,
        );

        // Channel bundle for the inspector body (shared between the
        // promoted-node FloatingPanel and the tiled Node pane). Declared
        // here so the tiled Node pane — rendered inside the workspace
        // SidePanel below — can write back through it; drained after all
        // panel paints near the end of `update`.
        let mut anchored_channels = AnchoredChannels::default();

        // Drain async node-meta / page-save slots before any Node-panel
        // paint so fresh data shows the same frame.
        self.drain_node_fetches();

        // Mirror "a node is promoted" onto AppState so the workspace's
        // open-state sync can mount/unmount the Node tile.
        self.state.node_panel_open = self.promoted_anchored_idx.is_some();

        // Tileable workspace — mounts a right-side panel hosting the
        // `egui_tiles::Tree` when at least one section / filter strip /
        // the Node panel is in `Placement::Tiled`. When zero panels are
        // tiled, the side panel is hidden so the canvas keeps full width.
        //
        // The Node tile's body needs App-owned data the workspace can't
        // borrow, so we hand it in as a closure. Built only when a node
        // is promoted AND tiled; otherwise the tile (if present) shows
        // its empty-state hint.
        {
            let node_promoted = self.promoted_anchored_idx;
            let node_tiled = self.state.node_panel_placement
                == crate::ui::tiles::Placement::Tiled;
            let node_meta = if node_tiled {
                self.promoted_anchored_meta.clone()
            } else {
                None
            };
            let edges_snapshot = if node_meta.is_some() {
                self.edges_snapshot(frame)
            } else {
                Vec::new()
            };
            let active_filters_snapshot = self.state.query.active_filters.clone();
            let color_by = self.state.style.color_by;
            let palette = self.state.style.palette;
            let mut tag_query = self.state.tag_browser_query.clone();

            // Destructure disjoint App fields so the node-body closure
            // borrows only those, leaving `self.state` free to hand to the
            // workspace mutably.
            let App {
                state,
                actions,
                layout_registry,
                perf,
                ids,
                metrics,
                field_index,
                page_viewer_states,
                page_viewer_markdown_cache,
                ..
            } = self;
            let ids = &*ids;
            let metrics = &*metrics;
            let field_index = field_index.as_ref();

            let mut node_body = |ui: &mut egui::Ui| {
                let (Some(idx), Some(meta)) = (node_promoted, node_meta.as_ref()) else {
                    return;
                };
                let max_h = (ui.available_height() - 8.0).max(120.0);
                render_node_body(
                    ui, max_h, idx, meta, ids, metrics, &edges_snapshot, color_by,
                    palette, &active_filters_snapshot, field_index,
                    page_viewer_states, page_viewer_markdown_cache, &mut tag_query,
                    &mut anchored_channels,
                );
            };
            let node_body_opt: Option<&mut dyn FnMut(&mut egui::Ui)> =
                if node_tiled { Some(&mut node_body) } else { None };
            // Title for the tiled Node pane header — the promoted node's
            // name, matching the floating panel chrome.
            let node_title_opt =
                node_meta.as_ref().map(node_title);
            ui::tiles::show_workspace_panel(
                ctx,
                state,
                actions,
                layout_registry,
                perf,
                node_body_opt,
                node_title_opt,
            );
            // Write back the (possibly edited) tag-browser query.
            self.state.tag_browser_query = tag_query;
        }

        // A node-tile close inside the workspace queued a dismissal.
        if std::mem::take(&mut self.state.node_panel_close_requested) {
            if let Some(idx) = self.promoted_anchored_idx {
                self.dismiss_promoted_node(idx);
            }
        }

        // Phase B central panel — now hosts the dockable Workspace
        // (tabs + splits via egui_dock). One initial "Graph" tab carries
        // the wgpu callback; the Welcome tab kind exists so the splitting
        // story is exercisable. Input/picking semantics match the pre-dock
        // central panel — see `ui::workspace::WorkspaceViewer::draw_graph_tab`.
        let load_msg: Option<String> = {
            let guard = self.load.lock().unwrap();
            ui::workspace::load_status_message(&*guard)
        };
        let mut wctx = ui::workspace::WorkspaceCtx {
            frame,
            loaded_into_gpu: self.loaded_into_gpu,
            load_msg: load_msg.as_deref(),
            invert_mouse_x: self.state.camera.invert_mouse_x,
            invert_mouse_y: self.state.camera.invert_mouse_y,
            pan_accel_t: &mut self.camera_pan_accel_t,
            input_events: &camera_input_events,
            focused_panel: self.state.focused_panel,
            canvas_rect: None,
            pointer_in_canvas: None,
            click: None,
            lmb_held: false,
            rmb_held: false,
            add_tab_requests: Vec::new(),
            split_requests: Vec::new(),
        };
        self.perf.begin_stage(StageId::EguiPaint);
        if self.state.canvas_mount.is_floating() {
            // Floating-canvas mode: CentralPanel is a flat dark fill,
            // the wgpu paint callback is invoked from inside the
            // floating "Graph" window. While popped out the egui_dock
            // tab strip is intentionally hidden — v1 limitation.
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(crate::ui::theme::palette::BLACK),
                )
                .show(ctx, |_ui| {});

            let mut canvas_open = true;
            let mut canvas_collapsed = self
                .state
                .collapsed_panels
                .contains(&ui::state::PanelId::Canvas);
            crate::ui::floating::FloatingPanel::new(
                ui::state::PanelId::Canvas,
                "Graph",
            )
            .default_pos([120.0, 80.0])
            .default_size([900.0, 600.0])
            .with_collapsed(&mut canvas_collapsed)
            .show(ctx, &mut canvas_open, |ui| {
                let mut viewer = ui::workspace::WorkspaceViewer { ctx: &mut wctx };
                viewer.draw_graph_tab(ui);
            });
            if canvas_collapsed {
                self.state.collapsed_panels.insert(ui::state::PanelId::Canvas);
            } else {
                self.state.collapsed_panels.remove(&ui::state::PanelId::Canvas);
            }
            // Snapshot the floating window's last-known rect back onto
            // `AppState::canvas_mount`. `FloatingPanel::show` only
            // returns the body inner; the window's outer rect lives in
            // egui's area memory under the same id the panel
            // constructs (`("floating", PanelId::Canvas)`). Observational
            // only — nothing reads this field yet (future "reset position"
            // command and cross-reload persistence).
            if let ui::state::CanvasMount::Floating { rect, .. } =
                &mut self.state.canvas_mount
            {
                let id = egui::Id::new(("floating", ui::state::PanelId::Canvas));
                if let Some(area_rect) = ctx.memory(|m| m.area_rect(id)) {
                    *rect = Some(area_rect);
                }
            }
            if !canvas_open {
                self.state.dock_canvas_back();
            }
        } else {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(egui::Color32::TRANSPARENT))
                .show(ctx, |ui| {
                    let mut viewer = ui::workspace::WorkspaceViewer { ctx: &mut wctx };
                    // Hide the dock tab bar when only one tab is mounted —
                    // the user complained about "an empty grey bar at the
                    // top". With a single Graph tab, the tab strip has
                    // nothing to do but the add-tab button, and the strip
                    // bg colour reads as an unexplained ribbon above the
                    // canvas. Collapsing it to zero height removes the
                    // ribbon entirely; users who split the workspace pick
                    // up tabs from the splits' result and get the bar back
                    // automatically (n_tabs > 1).
                    let n_tabs: usize = self
                        .state
                        .dock
                        .dock_state
                        .iter_all_tabs()
                        .count();
                    let force_show = self.state.dock_tab_strip_force_show;
                    self.state.dock_tab_strip_force_show = false;
                    let show_tab = force_show || n_tabs > 1;
                    let mut style = egui_dock::Style::from_egui(ui.style());
                    // Override ALL dock styling to match high-contrast theme
                    style.tab_bar.bg_fill = crate::ui::theme::palette::BLACK;
                    style.tab_bar.hline_color = crate::ui::theme::palette::BORDER;
                    style.tab_bar.fill_tab_bar = true;
                    // Kill any grey separators or borders
                    style.separator.color_idle = crate::ui::theme::palette::BORDER;
                    style.separator.color_hovered = crate::ui::theme::palette::WHITE;
                    style.separator.color_dragged = crate::ui::theme::palette::WHITE;
                    style.main_surface_border_stroke = egui::Stroke::new(0.0, egui::Color32::TRANSPARENT);
                    if !show_tab {
                        style.tab_bar.height = 0.0;
                        style.tab_bar.bg_fill = egui::Color32::TRANSPARENT;
                    }
                    egui_dock::DockArea::new(&mut self.state.dock.dock_state)
                        .show_add_buttons(show_tab)
                        .show_add_popup(show_tab)
                        .style(style)
                        .show_inside(ui, &mut viewer);
                });
        }
        self.perf.end_stage(StageId::EguiPaint);

        // Drain workspace requests collected during the DockArea pass.
        for kind in wctx.add_tab_requests.drain(..) {
            self.state.dock.push_tab(kind);
        }
        for req in wctx.split_requests.drain(..) {
            let new_node =
                egui_dock::Node::leaf(ui::workspace::Tab::new(req.new_tab));
            self.state.dock.dock_state.split(
                (req.surface, req.node),
                req.split,
                0.5,
                new_node,
            );
        }

        let click = wctx.click;
        let canvas_rect = wctx.canvas_rect;
        let pointer_in_canvas = wctx.pointer_in_canvas;

        self.prev_canvas_rect = canvas_rect;
        self.last_pointer_in_canvas = pointer_in_canvas;
        // Cursor-as-force is disabled. On compact layouts (e.g. fresh
        // topo-fisheye seeds, small graphs) attract-on-LMB pulls every
        // visible node into a single point, blanking the canvas until
        // spring + repulsion restore them. The GPU uniform plumbing and
        // the WGSL force block stay live (cheap; well-tested), but no
        // input ever sets `cursor_force_active`, so the radius=0 branch
        // in `apply_cursor_force` keeps the GPU's `cursor_radius` at 0
        // and the shader skips the cursor term entirely. To re-enable
        // later, restore the lmb/rmb dispatch here.
        self.cursor_force_active = 0.0;

        // Esc closes the modal — wired through the AppAction::Cancel
        // pulse drained at the top of `update`. The palette has its
        // own internal Esc handler (see ui::command_palette::show)
        // and runs first via egui's hover/focus order, so this only
        // fires when nothing else swallowed the press.
        if want_cancel {
            // Esc precedence: if the modal is open, close it;
            // otherwise, if a panel is focused, defocus it (return
            // scroll routing to the canvas). Single Esc does one
            // thing so the gesture stays predictable.
            if self.modal.open {
                self.modal.open = false;
                self.modal.current = None;
                self.modal.fetch_error = None;
            } else if self.state.focused_panel.is_some() {
                self.state.focused_panel = None;
            }
        }

        if let Some((rect, pos)) = click {
            // Coordinate-space chain for the click → node-pick path:
            // - `pos` is an egui::Pos2 in *logical* pixels, in screen-space
            //   relative to the egui root (top-left = (0,0)).
            // - `rect` is the *exact* tab-content rect that the wgpu callback
            //   painted into this frame (captured in workspace.rs at click
            //   time so it can't drift if the layout reflows next frame).
            // - `ndc_x`, `ndc_y`: NDC of the cursor inside the canvas, in
            //   [-1, 1] with y-up. NDC y is flipped from window y because
            //   wgpu/glam clip space is y-up while egui pixels are y-down.
            // The same rect's width/height feed `screen_px` in
            // GraphPipelines, which also drives camera.aspect — so the
            // projection matrix used by raycast() matches the rect we
            // painted into and the rect we hit-test against. (See
            // GraphPipelines::raycast for the projection / pick math.)
            let rect_w = rect.width().max(1.0);
            let rect_h = rect.height().max(1.0);
            let ndc_x = (pos.x - rect.left()) / rect_w * 2.0 - 1.0;
            let ndc_y = -((pos.y - rect.top()) / rect_h * 2.0 - 1.0);
            let click_hit = self
                .raycast_idx(frame, ndc_x, ndc_y, [rect_w, rect_h])
                .filter(|&idx| {
                    // Same gate as hover: in Filter mode, clicks on
                    // filtered-out nodes are no-ops. Without this, the
                    // user can sticky-focus a filtered node and the
                    // promoted anchored card opens for an invisible
                    // target — same "reappears via panel chrome" bug.
                    if !matches!(
                        self.state.filter_behavior,
                        crate::ui::state::FilterBehavior::Filter
                    ) {
                        return true;
                    }
                    self.field_index
                        .as_ref()
                        .and_then(|fi| fi.matches(&self.state.query.active_filters))
                        .map(|set| set.contains(&idx))
                        .unwrap_or(true)
                });
            if let Some(idx) = click_hit {
                if let Some(id) = self.id_for_idx(idx) {
                    log::info!(
                        "[graph-renderer] click hit node idx={} id={}",
                        idx,
                        id
                    );
                    self.selected_node_idx = Some(idx);
                    // Sticky focus: click locks focus on this node. Hover
                    // is suppressed while sticky is set.
                    self.focus_sticky_idx = Some(idx);
                    // Promote the anchored panel for this node into
                    // the focused window — scroll wheel events will
                    // route to its inner ScrollArea (when expanded),
                    // not to the canvas zoom path.
                    self.state.focused_panel = Some(
                        crate::ui::state::FocusedPanel::AnchoredNode(idx),
                    );
                    // Promote the anchored panel: clicking a node makes
                    // the floating card sticky (interactable, persists
                    // until X-ed). Option semantics handle swap-on-other-
                    // node automatically. We intentionally do NOT clear
                    // `promoted_anchored_idx` in the background-click
                    // branch below — clicking empty canvas should not
                    // dismiss a promoted panel.
                    let prev_promoted = self.promoted_anchored_idx;
                    self.promoted_anchored_idx = Some(idx);
                    if prev_promoted != Some(idx) {
                        self.state.frontend_events.push(
                            "anchored:promote",
                            format!("idx={idx} id={id}"),
                        );
                        // Swap: drop the old meta + kick a fetch for
                        // the new id. Reuse the hover preview's cached
                        // meta when it already matches to avoid an
                        // immediate empty-render flash.
                        self.promoted_anchored_meta = self
                            .hover_preview_meta
                            .as_ref()
                            .filter(|m| m.id == id)
                            .cloned();
                        self.kick_off_promoted_anchored_fetch(id.clone());
                    }
                    // (Previously: if the right-side inspector was
                    // collapsed, a node-click force-opened it. The inspector
                    // no longer exists as a separate surface — the promoted
                    // anchored panel is the post-click default — so this
                    // branch is gone.)
                    self.kick_off_node_fetch(id);
                }
            } else {
                // Click on empty canvas → clear sticky focus AND
                // defocus any panel so the canvas regains scroll
                // routing (focused_panel = None means "canvas is the
                // focused surface" per the gate in ui::workspace).
                self.focus_sticky_idx = None;
                self.state.focused_panel = None;
            }
        }

        // Hover-driven focus (throttled). Sticky click takes precedence.
        self.update_hover_focus(frame, pointer_in_canvas, canvas_rect);
        // Hover-preview card delay/fetch state machine. Reads
        // `focus_hover_idx` set above. The actual paint happens in
        // `show_hover_preview` at the end of `update` so it sits on
        // top of the existing UI layers.
        self.tick_hover_preview(pointer_in_canvas);

        // (Inspector-requested selection drain moved to after
        //  `show_hover_preview` — the inspector body is now embedded
        //  in the anchored panel, so its channels are populated
        //  during that paint pass and drained afterwards.)

        // Filter chip strip — sits above the canvas, below the modal.
        ui::filter_strip::show_floating(ctx, &mut self.state);

        // Draw the modal — last so it stacks above the central panel.
        // Resolve the canvas tint for the focused node so the modal's
        // metadata badges match whatever swatch StyleState::color_by
        // is painting it with on the canvas.
        let modal_tint = self.modal.current.as_ref().and_then(|meta| {
            let idx = self.ids.iter().position(|s| s == &meta.id)? as u32;
            crate::data::node_color_for_key(
                self.state.style.color_by.metric_key(),
                idx,
                &self.metrics,
                self.state.style.palette,
            )
        });
        let action = ui::modal::show_modal_with(
            ctx,
            &mut self.modal,
            &self.state.query.active_filters,
            modal_tint,
        );
        // Prefer the focus_node channel — it folds camera + sidebar in one
        // helper. Fall back to plain navigate (kick a fetch only) when the
        // modal didn't tag a focus target (e.g. ticket-id chips that don't
        // resolve to a graph node).
        if let Some(id) = action.focus_node {
            self.focus_node_by_id(frame, &id);
        } else if let Some(target) = action.navigate_to {
            self.kick_off_node_fetch(target);
        }
        if let Some((field, value)) = action.toggle_filter {
            self.state.query.toggle_field_filter(&field, &value);
        }
        if let Some(href) = action.open_url {
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(window) = web_sys::window() {
                    let _ = window.open_with_url_and_target(&href, "_blank");
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                log::info!("[graph-renderer] open url (native no-op): {href}");
            }
        }

        // ---- Per-frame wiring loops -----------------------------------
        self.kick_off_pending_searches();

        self.perf.begin_stage(StageId::ApplyStyle);
        self.apply_style_to_gpu(frame);
        self.perf.end_stage(StageId::ApplyStyle);

        self.perf.begin_stage(StageId::LayoutDispatch);
        self.apply_layout_to_gpu(frame);
        self.perf.end_stage(StageId::LayoutDispatch);

        // Initial-seed section (Layout panel): drain freshly evaluated seed
        // positions, if any, into the live GPU positions buffer.
        self.drain_seed_positions(frame);

        // Scrubbable-timeline tick: capture live positions into the ring (when
        // live) + push the scrubbed frame to the GPU (when paused). Runs after
        // seed drain so a fresh seed lands before we capture this frame.
        self.tick_timeline(frame);

        // Metrics panel: drain its one-shot compute flag (no-op unless the user
        // pressed Compute), reading positions/edges from the live pipeline.
        self.drain_metrics_request(frame);

        // Remote-engine picker (Layout section) — drain one-shot intent
        // flags into async HTTP. Cheap no-op on the common frame where no
        // flag is set.
        self.drain_compute_engine_requests();

        self.perf.begin_stage(StageId::ApplyEffects);
        self.apply_focus_to_gpu(frame);
        self.apply_camera_to_gpu(ctx, frame);
        self.apply_cursor_force(frame);
        self.tick_post_click_cooldown(frame);
        self.perf.end_stage(StageId::ApplyEffects);

        // Promoted-node FloatingPanel paint pass (floating placement only;
        // the tiled placement rendered in the workspace SidePanel above).
        // Writes back through the same `anchored_channels` declared earlier.
        self.render_node_panel_floating(ctx, frame, &mut anchored_channels);

        // Hover-preview card paint pass — runs after all other UI
        // layers so the card sits on top of canvas + sidebars. Cheap:
        // no-op when `hover_preview_open == false`.
        self.show_hover_preview(ctx, frame);

        // Post-anchored-panel inspector channel drain.
        if let Some(idx) = anchored_channels.requested_selection.take() {
            self.selected_node_idx = Some(idx);
            if let Some(id) = self.id_for_idx(idx) {
                log::info!(
                    "[graph-renderer] inspector selected idx={} id={}",
                    idx, id,
                );
                self.focus_node_by_id(frame, &id);
            }
        }
        if let Some((f, v)) = anchored_channels.requested_filter_toggle.take() {
            self.state.query.toggle_field_filter(&f, &v);
        }
        if let Some(id) = anchored_channels.requested_focus_node.take() {
            self.focus_node_by_id(frame, &id);
        } else if let Some(target) = anchored_channels.requested_navigate.take() {
            self.kick_off_node_fetch(target);
        }
        if let Some((node_id, path, body)) = anchored_channels.requested_page_save.take() {
            self.kick_off_page_save(node_id, path, body);
        }
        if let Some(href) = anchored_channels.requested_open_url.take() {
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(window) = web_sys::window() {
                    let _ = window.open_with_url_and_target(&href, "_blank");
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                log::info!("[graph-renderer] open url (native no-op): {href}");
            }
        }

        // Detect filter-set empty<->non-empty transitions and auto-flip
        // the user's FocusMode so a badge click engages focus dim the same
        // way clicking a node does. Restore the saved mode when the filter
        // set drains back to empty. Runs every frame so any surface that
        // mutates `active_filters` (inspector badges, modal badges, filter
        // chip strip, query builder) participates without per-call-site
        // bookkeeping.
        self.handle_filter_focus_auto_flip();

        self.perf.begin_stage(StageId::ApplySelection);
        self.apply_selection(frame);
        self.apply_focus_set_to_gpu(frame);
        self.apply_hover_to_gpu(frame);
        self.apply_edge_hover_to_gpu(frame);
        self.perf.end_stage(StageId::ApplySelection);

        self.perf.begin_stage(StageId::RefreshStats);
        self.refresh_stats(frame);
        self.perf.end_stage(StageId::RefreshStats);

        // Mirror sim/backend metadata into the perf collector for the
        // Debug section's running/halted badge + backend label.
        self.perf.last_halted = matches!(
            self.state.sim_status,
            ui::state::SimStatus::Settled
        );
        if self.perf.last_layout_id != self.state.layout.active {
            self.perf.last_layout_id = self.state.layout.active.clone();
        }

        // Drive continuous repaint only when something is actually
        // changing frame-to-frame. Otherwise let egui's input-driven
        // repaints handle redraws — saves enormous GPU work on settled
        // graphs with the palette closed.
        let sim_settled = matches!(self.state.sim_status, ui::state::SimStatus::Settled);
        let needs_continuous = !sim_settled
            || self.palette_state.open
            || !self.loaded_into_gpu
            || self.cursor_force_active.abs() > 0.0
            // While paused-scrubbing we re-push the scrub frame each frame to
            // hold the canvas against the still-running sim — keep repainting.
            || self.state.timeline.is_paused();
        // Treat "any pointer activity this frame" as user input — force
        // an immediate next-frame repaint so input feels snappy even if
        // the warm-throttle below would otherwise slow us down.
        let has_user_input = ctx.input(|i| {
            i.pointer.any_pressed()
                || i.pointer.any_released()
                || i.pointer.is_moving()
        });
        if needs_continuous {
            // Always repaint immediately while the sim or user is active.
            // The earlier "warm-throttle" (drop to 20fps when KE is high)
            // saved GPU but the user perceived the throttle on/off
            // transition as the layout speeding up and slowing down. With
            // a fixed sim dt + steps_per_call, frame interval changes
            // translate directly into apparent motion velocity changes.
            // Constant-cadence repainting is worth the extra GPU cycles.
            let _ = has_user_input;
            ctx.request_repaint();
        } else {
            // Light tick so a fresh user action (e.g. an action instance
            // mutating state) isn't held up for an arbitrary time.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        // -- Auto-snapshot timeline (cross-platform) -------------------
        //
        // Hash AppState (the same JSON-hash the wasm persistence layer
        // uses) and, on a debounced cadence, push a snapshot whenever
        // the hash diffs from the previously-observed value. We drain
        // `snapshot_source` every tick — whether or not a diff lands —
        // so a label set by a no-op call doesn't get stuck and tag the
        // next unrelated change with the wrong source.
        self.tick_snapshots();

        // -- WASM debounced sessionStorage persistence -----------------
        //
        // Detect a state mutation via JSON-hash diff (cheap enough for a
        // panel-state-sized struct; revisit if it shows up in profiles)
        // and, when dirty, flush at most once per PERSIST_DEBOUNCE window
        // to sessionStorage only. `App::save` still mirrors to
        // eframe::Storage on eframe's own ~30s cadence.
        #[cfg(target_arch = "wasm32")]
        {
            use std::hash::{Hash, Hasher};
            use std::time::Duration;
            const PERSIST_DEBOUNCE: Duration = Duration::from_millis(800);

            if let Ok(json) = serde_json::to_string(&self.state) {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                json.hash(&mut h);
                let now_hash = h.finish();
                if Some(now_hash) != self.state_pushed_hash {
                    self.state_dirty = true;
                    self.state_pushed_hash = Some(now_hash);
                }
            }

            if self.state_dirty {
                let now = web_time::Instant::now();
                let elapsed = self
                    .last_persist_at
                    .map(|t| now.duration_since(t))
                    .unwrap_or(Duration::from_secs(10));
                if elapsed > PERSIST_DEBOUNCE {
                    ui::persist::save_to_sessionstorage_only(&self.state);
                    self.state_dirty = false;
                    self.last_persist_at = Some(now);
                }
            }
        }

        self.perf.end_frame(self.last_observed_max_ke);
    }
}

impl App {
    /// Auto-snapshot the current `AppState` if it differs from the
    /// previously observed hash, throttled by `SNAPSHOT_INTERVAL`. The
    /// label is drained from `snapshot_source` (set by mutation sites
    /// that bother to attribute themselves) or falls back to `"misc"`.
    ///
    /// `snapshot_source` is drained on every call regardless of
    /// whether a diff is observed, so a label set by a no-op mutation
    /// never lingers to mislabel a later, unrelated diff.
    fn tick_snapshots(&mut self) {
        use std::hash::{Hash, Hasher};
        use std::time::Duration;
        const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(250);

        // Drain attribution label up-front. Even if the diff check
        // below decides not to snapshot, we don't want a stale label
        // bleeding into the next genuine mutation.
        let drained_source = self.state.snapshot_source.take();

        let json = match serde_json::to_string(&self.state) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        json.hash(&mut h);
        let now_hash = h.finish();

        let prev = self.snapshot_hash;
        self.snapshot_hash = Some(now_hash);

        // First observation: seed the hash but don't push — the
        // `default` / `restored` entries from `App::new` cover the
        // starting state.
        if prev.is_none() || prev == Some(now_hash) {
            return;
        }

        // Debounce bursts of slider-drag style mutations.
        let now = web_time::Instant::now();
        if let Some(last) = self.last_snapshot_at {
            if now.duration_since(last) < SNAPSHOT_INTERVAL {
                // Restore the drained label so the next eligible
                // tick within the same burst can still pick it up.
                if self.state.snapshot_source.is_none() {
                    self.state.snapshot_source = drained_source;
                }
                return;
            }
        }

        let source = drained_source.unwrap_or_else(|| "misc".to_string());
        self.state.snapshot_now(source);
        self.last_snapshot_at = Some(now);
    }

    /// Append a Filter card from a modal badge click. If the last card
    /// isn't already a connector or paren-open, prepend an AND connector
    /// so the new filter joins the chain. Auto-focuses the Filter section
    /// in the sidebar so the user sees the addition land.
    fn append_filter_card(&mut self, field: String, value: String) {
        use crate::ui::query::{Card, ConnectorOp};
        let cards = &mut self.state.query.cards;
        let needs_connector = match cards.last() {
            None => false,
            Some(Card::Connector { .. }) | Some(Card::ParenOpen) | Some(Card::Not) => false,
            _ => true,
        };
        if needs_connector {
            cards.push(Card::Connector { op: ConnectorOp::And });
        }
        cards.push(Card::Filter {
            field,
            op: crate::ui::query::Op::Eq,
            value,
        });
        self.state.set_section_open(ui::Section::Filter, true);
    }

    /// Walk the QueryModel and spawn a /search?q= fetch for any Search
    /// card whose value isn't yet in the cache and isn't already inflight.
    fn kick_off_pending_searches(&mut self) {
        let pending = self.state.query.pending_searches();
        let cache_keys: Vec<String> = {
            let g = self.search_cache.lock().unwrap();
            g.keys().cloned().collect()
        };
        for q in pending {
            if cache_keys.iter().any(|k| k == &q) {
                continue;
            }
            if !self.search_inflight.insert(q.clone()) {
                continue;
            }
            let cache = self.search_cache.clone();
            let id_to_idx = self.id_to_idx.clone();
            let client = ApiClient::new(self.base_url.clone());
            let q_owned = q.clone();
            let sink = self.progress.sink();
            let label = format!("search '{}'", short_label(&q_owned));
            spawn_async(async move {
                let task = sink.start("search", label);
                match client.search(&q_owned).await {
                    Ok(results) => {
                        let mut set: HashSet<u32> = HashSet::new();
                        for id in results.ids {
                            if let Some(&idx) = id_to_idx.get(&id) {
                                set.insert(idx);
                            }
                        }
                        let hits = set.len();
                        cache.lock().unwrap().insert(q_owned, set);
                        sink.finish(task);
                        sink.info("search", format!("{hits} hits"));
                    }
                    Err(e) => {
                        log::warn!("[graph-renderer] /search failed: {e}");
                        sink.fail(task, e);
                        // Insert empty so we don't loop forever on bad query.
                        cache.lock().unwrap().insert(q_owned, HashSet::new());
                    }
                }
            });
        }
    }

    fn style_key(&self) -> (
        SizeBy,
        ColorBy,
        ShapeBy,
        u32,
        u32,
        EdgeColorBy,
        [u32; 4],
        crate::data::PaletteId,
        crate::ui::state::CommunitySource,
    ) {
        let ec = self.state.style.edge_color;
        (
            self.state.style.size_by,
            self.state.style.color_by,
            self.state.style.shape_by,
            self.state.style.size_mul.to_bits(),
            (self.state.style.log_scale_size as u32),
            self.state.style.edge_color_by,
            [
                ec[0].to_bits(),
                ec[1].to_bits(),
                ec[2].to_bits(),
                ec[3].to_bits(),
            ],
            self.state.style.palette,
            self.state.style.community_source,
        )
    }

    fn apply_style_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let key = self.style_key();
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let recompute_buffers = self.prev_style_key != Some(key);

        // Edge style is cheap (uniform write) — push it every frame.
        // Sliders read live, no change-detect needed.
        let s = &self.state.style;
        {
            let mut renderer = wgpu_state.renderer.write();
            if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
                pipes.set_edge_style(
                    s.edge_color,
                    s.edge_alpha_mul,
                    (s.edge_dist_min, s.edge_dist_max),
                    s.edge_min_transparency,
                    s.edge_width * apply_size_scale(s.edge_size_mul, s.log_scale_size),
                    s.edge_fade_floor,
                );
                pipes.set_shader_intensity(s.shader_intensity);
            }
        }

        if !recompute_buffers {
            return;
        }
        let queue = wgpu_state.queue.clone();
        let n = self.ids.len();
        // Metrics view honours `ColorBy::Tag` / `EdgeColorBy::Tag` and
        // the `community_source == Tag` override in one shot.
        let mv = self.metrics_view();
        let colors = data::colors_from_metric(
            self.state.style.color_by.metric_key(),
            mv.as_ref(),
            n,
            self.state.style.palette,
        );
        let sizes = data::sizes_from_metric(
            self.state.style.size_by.metric_key(),
            &self.metrics,
            n,
            apply_size_scale(self.state.style.size_mul, self.state.style.log_scale_size),
        );
        let shapes = data::shapes_from_metric(
            self.state.style.shape_by.metric_key(),
            mv.as_ref(),
            n,
        );
        let edge_color_by = self.state.style.edge_color_by;
        let edge_fallback = self.state.style.edge_color;
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            pipes.update_colors(&queue, colors);
            pipes.update_sizes(&queue, sizes);
            pipes.update_shape_ids(&queue, shapes);
            // Edge colors: when EdgeColorBy::None, push an all-1.0 buffer
            // so the uniform `edge_color` rules unchanged. Otherwise build
            // per-edge tints from the chosen categorical metric.
            let n_edges = pipes.n_edges() as usize;
            if n_edges > 0 {
                let edge_colors = if edge_color_by == EdgeColorBy::None {
                    let mut v = Vec::with_capacity(n_edges * 4);
                    for _ in 0..n_edges {
                        v.extend_from_slice(&edge_fallback);
                    }
                    v
                } else {
                    data::edge_colors_from_metric(
                        edge_color_by.metric_key(),
                        mv.as_ref(),
                        n,
                        pipes.edges_cpu(),
                        edge_fallback,
                        self.state.style.palette,
                    )
                };
                pipes.update_edge_colors(&queue, edge_colors);
            }
        }
        drop(mv);
        self.prev_style_key = Some(key);
        // Force a selection re-push so the dim alpha overlays the new colours.
        self.prev_selected_hash = None;
    }

    /// Stable hash of the active layout id + its settings JSON. Drives the
    /// per-frame change-detect that gates the JSON push to the GPU layout.
    fn layout_key(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.state.layout.active.hash(&mut h);
        let json_str = self
            .state
            .layout
            .settings
            .get(&self.state.layout.active)
            .and_then(|v| serde_json::to_string(v).ok())
            .unwrap_or_default();
        json_str.hash(&mut h);
        h.finish()
    }

    /// Drain the Layout section's "Remote engine" picker intents. The
    /// section can't reach the `ApiClient` (it only gets `&mut AppState`),
    /// so it raises one-shot flags on `state.compute` and we service them
    /// here against the shared snapshot latch.
    ///
    /// - `refresh_requested` → GET `/compute/engines`, store result.
    /// - `select` → PUT `/compute/layout` then re-fetch engines so the
    ///   combo reflects the new `active`.
    fn drain_compute_engine_requests(&mut self) {
        let refresh = std::mem::take(&mut self.state.compute.refresh_requested);
        let select = self.state.compute.select.take();

        if select.is_none() && !refresh {
            return;
        }

        let slot = self.state.compute.snapshot.clone();
        let base = self.base_url.clone();

        if let Some(layout_id) = select {
            // Switch engine, then refresh the list regardless of switch
            // outcome so the UI shows the resulting `active` (or surfaces
            // a stale state the user can retry).
            spawn_async(async move {
                let client = ApiClient::new(base);
                if let Err(e) = client.set_compute_layout(&layout_id, None).await {
                    log::warn!("set_compute_layout({layout_id}) failed: {e}");
                }
                let res = client.list_compute_engines().await;
                if let Ok(mut g) = slot.lock() {
                    *g = Some(res);
                }
            });
        } else if refresh {
            spawn_async(async move {
                let client = ApiClient::new(base);
                let res = client.list_compute_engines().await;
                if let Ok(mut g) = slot.lock() {
                    *g = Some(res);
                }
            });
        }
    }

    /// Drain the Metrics-panel one-shot compute flags: read CPU positions +
    /// edges from the live pipeline and compute layout-quality metrics into
    /// `AppState`. Mirrors the `layout_solve_requested` pattern. Cheap edge
    /// metrics always; the O(n²) full-stress pass only when explicitly requested
    /// AND the graph is small enough to keep the UI responsive.
    fn drain_metrics_request(&mut self, frame: &mut eframe::Frame) {
        // Live mode recomputes the cheap (edge-based) metrics every frame.
        let want = std::mem::take(&mut self.state.metrics.compute_requested) || self.state.metrics.auto;
        let want_full = std::mem::take(&mut self.state.metrics.compute_full_requested);
        if !want {
            return;
        }

        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };
        let renderer = wgpu_state.renderer.read();
        let Some(pipes) = renderer.callback_resources.get::<GraphPipelines>() else {
            return;
        };
        let positions = pipes.positions_cpu();
        let edges = pipes.edges_cpu();
        let n = positions.len() / 3;
        let edge_pairs: Vec<(u32, u32)> =
            edges.chunks_exact(2).map(|c| (c[0], c[1])).collect();

        // Cheap, O(E): edge-length CV + edge-only scale-normalized stress
        // (uniform unit target — no per-frame terms allocation).
        let edge_length_cv = graph_layouts::metrics::edge_length_cv(positions, &edge_pairs);
        let edge_stress =
            graph_layouts::metrics::scale_normalized_stress_uniform(positions, &edge_pairs);

        // Expensive metrics (O(n²) stress, O(E²) crossings) only on the explicit
        // "+ full stress" request, each gated by size; otherwise preserve the
        // last computed values across cheap/auto recomputes.
        const MAX_FULL_NODES: usize = 2000;
        const MAX_CROSSING_EDGES: usize = 20_000;
        let (full_stress, crossings) = if want_full {
            let fs = if n > 0 && n <= MAX_FULL_NODES {
                Some(graph_layouts::metrics::all_pairs_normalized_stress(
                    positions,
                    &edge_pairs,
                    n,
                ))
            } else {
                None
            };
            let cr = if edge_pairs.len() <= MAX_CROSSING_EDGES {
                Some(graph_layouts::metrics::edge_crossings(positions, &edge_pairs))
            } else {
                None
            };
            (fs, cr)
        } else {
            let prev = self.state.metrics.last;
            (prev.and_then(|s| s.full_stress), prev.and_then(|s| s.crossings))
        };

        self.state.metrics.last = Some(crate::ui::state::MetricsSnapshot {
            n_nodes: n as u32,
            n_edges: edge_pairs.len() as u32,
            edge_length_cv,
            edge_stress,
            full_stress,
            crossings,
        });
    }

    fn apply_layout_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let key = self.layout_key();
        // A pending Solve must always run (re-roll Random with the same
        // settings produces the same key), so don't short-circuit on it.
        let solve_requested = std::mem::take(&mut self.state.layout_solve_requested);
        if !solve_requested && self.prev_layout_key == Some(key) {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };

        let active_id = self.state.layout.active.clone();
        // Lazy-init the JSON to the active factory's defaults if missing
        // — fresh state has an empty settings map, and pushing `Null`
        // into the layout would fail the deserialise on the other side.
        if !self.state.layout.settings.contains_key(&active_id) {
            if let Some(factory) = self.layout_registry.get(&active_id) {
                // Seed JSON. For the gpu-force algorithm, swap the
                // hand-anchored Default block for `for_n_nodes(N)` so
                // the spring_len / repulsion match the loaded graph
                // size instead of the ~10k-node anchor that produces
                // a dense ball on smaller vaults.
                let initial_json = if active_id == "gpu-force" {
                    serde_json::to_value(GpuForceOptions::for_n_nodes(self.ids.len()))
                        .unwrap_or_else(|_| factory.default_settings())
                } else {
                    factory.default_settings()
                };
                self.state
                    .layout
                    .settings
                    .insert(active_id.clone(), initial_json);
            }
        }
        // Snapshot the JSON once so we don't borrow `self.state` and the
        // wgpu renderer at the same time.
        let json_owned = self
            .state
            .layout
            .settings
            .get(&active_id)
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        // For the gpu-force backend, derive `repulsion_radius` from the
        // current `spring_len` exactly the way the legacy path did
        // (4 * spring_len). Done by round-tripping through GpuForceOptions
        // so we don't muck with arbitrary backend JSON.
        let json_owned = if active_id == "gpu-force" {
            match serde_json::from_value::<GpuForceOptions>(json_owned.clone()) {
                Ok(mut opts) => {
                    opts.repulsion_radius = (4.0 * opts.spring_len).max(1.0);
                    // Cap steps_per_call to prevent excessive GPU load and overheating
                    // Persisted high values from older configurations can cause sustained
                    // GPU usage leading to thermal issues. This cap ensures tuned values
                    // survive while preventing excessive values.
                    const MAX_STEPS_PER_CALL: u32 = 16;
                    if opts.steps_per_call > MAX_STEPS_PER_CALL {
                        opts.steps_per_call = MAX_STEPS_PER_CALL;
                    }
                    serde_json::to_value(&opts).unwrap_or(json_owned)
                }
                Err(_) => json_owned,
            }
        } else {
            json_owned
        };

        let active_changed = self
            .prev_active_layout_id
            .as_deref()
            .map(|prev| prev != active_id.as_str())
            .unwrap_or(false);

        // Decode the gpu-force seed_mode out of the JSON if applicable so we
        // can detect a seed-mode change. `set_options` doesn't re-precompute,
        // so a plain settings push can't actually re-seed — we have to force
        // a swap.
        let new_seed_mode: Option<SeedMode> = if active_id == "gpu-force" {
            serde_json::from_value::<GpuForceOptions>(json_owned.clone())
                .ok()
                .map(|o| o.seed_mode)
        } else {
            None
        };
        let seed_mode_changed = match (&self.prev_seed_mode, &new_seed_mode) {
            (Some(prev), Some(curr)) => prev != curr,
            // First gpu-force apply, or moving onto gpu-force from elsewhere
            // — covered by `active_changed`, no need to double-trigger.
            _ => false,
        };

        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            if let Some(factory) = self.layout_registry.get(&active_id) {
                match factory.kind() {
                    graph_layouts::LayoutKind::Physics => {
                        if active_changed || seed_mode_changed {
                            pipes.swap_physics_layout(&device, &queue, factory, &json_owned);
                        } else {
                            pipes.set_physics_layout_settings_json(&json_owned);
                        }
                        // Physics-side Wake button reuses the
                        // `layout_solve_requested` one-shot flag (we
                        // share the channel between Solve and Wake to
                        // avoid threading another bool through the
                        // sidebar). For physics layouts the flag means
                        // "reignite": call wake() on the active layout.
                        if solve_requested {
                            pipes.wake_physics_layout();
                        }
                    }
                    graph_layouts::LayoutKind::Static => {
                        // Solve when the algorithm just changed to a static
                        // backend, or when the Solve button was pressed.
                        // Settings-only edits don't auto-solve — the user
                        // hits Solve to commit them.
                        if active_changed || solve_requested {
                            if let Err(e) =
                                pipes.run_static_solve(&queue, factory, &json_owned)
                            {
                                log::warn!(
                                    "[graph-renderer] run_static_solve: {e}"
                                );
                            }
                        }
                    }
                }
            }
        }
        self.prev_layout_key = Some(key);
        self.prev_active_layout_id = Some(active_id);
        self.prev_seed_mode = new_seed_mode;
    }

    /// Scrubbable-timeline tick (Phase P3). Runs every frame:
    ///
    /// 1. Reconcile the ring's depth with `state.timeline.depth` (a knob change
    ///    evicts oldest frames immediately).
    /// 2. Mirror the ring's frame count + byte budget into `state.timeline` so
    ///    the section can size the scrub slider + render the readout.
    /// 3. While **live**, capture the current CPU positions into the ring on the
    ///    capture stride. While **paused**, do NOT capture — we freeze the
    ///    visible buffer head so the user can scrub the existing history without
    ///    the incoming stream sliding it out from under them. (The live sim
    ///    keeps running on the GPU; we simply stop *consuming* it into the ring,
    ///    keeping pause/resume simple + correct per the task constraint.)
    /// 4. While **paused**, write the selected buffered frame to the GPU via
    ///    `GraphPipelines::set_positions` so the canvas shows that moment. On
    ///    resume, push the live head once so the canvas snaps back to "now".
    fn tick_timeline(&mut self, frame: &mut eframe::Frame) {
        // 1. Reconcile capture knobs.
        let depth = self.state.timeline.depth.max(1);
        let stride = self.state.timeline.stride.max(1);
        if self.timeline_prev_knobs != Some((depth, stride)) {
            self.frame_ring.set_depth(depth);
            self.timeline_prev_knobs = Some((depth, stride));
        }

        if !self.loaded_into_gpu {
            self.state.timeline.buffered_len = self.frame_ring.len();
            self.state.timeline.buffered_bytes = self.frame_ring.approx_bytes();
            return;
        }

        let paused = self.state.timeline.is_paused();

        // 3. Capture from the live CPU position mirror (only while live).
        if !paused {
            self.timeline_capture_idx = self.timeline_capture_idx.wrapping_add(1);
            if self.timeline_capture_idx % stride as u64 == 0 {
                if let Some(wgpu_state) = frame.wgpu_render_state() {
                    let renderer = wgpu_state.renderer.read();
                    if let Some(pipes) = renderer.callback_resources.get::<GraphPipelines>() {
                        let positions = pipes.positions_cpu();
                        if !positions.is_empty() {
                            self.frame_ring.push(positions);
                        }
                    }
                }
            }
        }

        // 2. Mirror buffer stats back for the section UI.
        self.state.timeline.buffered_len = self.frame_ring.len();
        self.state.timeline.buffered_bytes = self.frame_ring.approx_bytes();

        // 4. Apply a scrubbed frame to the GPU.
        //
        // While paused we re-push the selected frame EVERY frame. The GPU sim
        // is still stepping (`compute_step` always runs), so `set_positions`
        // both holds the canvas on the scrub frame and re-seeds the layout from
        // it — without this the sim would drift away from the frozen frame
        // between pushes. This is the simple+correct first slice; a future
        // "halt the layout while paused" optimization avoids the per-frame
        // re-init for large graphs.
        let seek_dirty = std::mem::take(&mut self.state.timeline.seek_dirty);
        if paused {
            let max = self.frame_ring.len().saturating_sub(1);
            let idx = self.state.timeline.current_idx().min(max);
            if self.frame_ring.len() > 0 {
                if let Some(positions) = self.frame_ring.get(idx) {
                    self.push_positions_to_gpu(frame, &positions);
                    self.timeline_pushed_idx = Some(idx);
                }
            }
            let _ = seek_dirty; // paused path always pushes; flag is irrelevant
        } else {
            // Live: if we just resumed from a paused scrub, snap the canvas
            // back to the newest buffered frame so the visible state matches
            // the still-running sim instead of the frozen scrub frame.
            if seek_dirty {
                if let Some(positions) = self.frame_ring.latest() {
                    self.push_positions_to_gpu(frame, &positions);
                }
            }
            self.timeline_pushed_idx = None;
        }
    }

    /// Write an absolute position frame straight into the live GPU positions
    /// buffer via `GraphPipelines::set_positions`. Shared by the timeline scrub
    /// path; mirrors `drain_seed_positions`'s wgpu access pattern.
    fn push_positions_to_gpu(&mut self, frame: &mut eframe::Frame, positions: &[f32]) {
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            if let Err(e) = pipes.set_positions(&device, &queue, positions) {
                log::warn!("[graph-renderer] timeline scrub set_positions: {e}");
            }
        }
    }

    /// Drain the Initial-seed section's one-shot: if the user applied a seed
    /// (built-in or custom Nix), `state.seed.pending` holds the freshly
    /// evaluated `[x,y,z]` positions. Write them straight into the live GPU
    /// positions buffer via `GraphPipelines::set_positions`, which also
    /// re-initialises the active physics layout so the sim resumes from the
    /// seed. The "No seed" strategy never sets `pending`, so this is a no-op.
    fn drain_seed_positions(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let Some(positions) = self.state.seed.pending.take() else {
            return;
        };
        // Flatten [[x,y,z]] -> [x,y,z,...].
        let flat: Vec<f32> = positions.iter().flat_map(|p| p.iter().copied()).collect();

        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            if let Err(e) = pipes.set_positions(&device, &queue, &flat) {
                log::warn!("[graph-renderer] drain_seed_positions: {e}");
                self.state.seed.editor.error = Some(e);
                self.state.seed.editor.status = None;
            }
        }
    }

    fn focus_key(&self) -> u64 {
        let f = &self.state.focus;
        let bits = [
            f.distance.to_bits(),
            f.thickness.to_bits(),
            f.blur.to_bits(),
            f.max_coc.to_bits(),
            f.dof_enabled as u32,
        ];
        let mut h: u64 = 0;
        for b in bits {
            h = h.wrapping_mul(31).wrapping_add(b as u64);
        }
        h
    }

    fn apply_focus_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let key = self.focus_key();
        if self.prev_focus_key == Some(key) {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            let f = &self.state.focus;
            let plane_z = pipes.camera.position.z - f.distance;
            // DoF off → push a sentinel thickness so node.wgsl's
            // `focus_thickness < 1e6` gate stays false for every node
            // (sharp fragment path, no bokeh quad inflation).
            let effective_thickness = if f.dof_enabled { f.thickness } else { 1.0e9 };
            pipes.set_focus_plane(plane_z, effective_thickness);
            pipes.set_dof_params(f.blur, f.max_coc);
        }
        self.prev_focus_key = Some(key);
    }

    fn apply_camera_to_gpu(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() else {
            return;
        };
        if self.state.camera.follow_centroid {
            if let Some(c) = pipes.centroid() {
                // Look-toward c: keep current distance along forward, retarget.
                let f = pipes.camera.forward();
                let dist = (c - pipes.camera.position).length().max(50.0);
                pipes.camera.position = c - f * dist;
            }
        }
        if self.state.camera.fit_to_window {
            // Auto-refit ONLY on actual window resize. Use ctx.screen_rect
            // (the egui-owned full surface) instead of the canvas rect,
            // because the canvas rect *also* shifts when the user opens
            // / closes a sidebar section — and refitting on a sidebar
            // toggle made the camera jump every time a button was
            // pressed.  Manual refit via `F` / Ctrl+P → Fit Camera.
            let screen = ctx.screen_rect().size();
            let screen_changed = match self.last_fit_screen {
                None => false, // initial fit done in load(); skip first frame
                Some(prev) => (prev - screen).abs().max_elem() > 1.0,
            };
            if screen_changed {
                pipes.fit_camera();
            }
            self.last_fit_screen = Some(screen);
        }
    }

    fn cursor_key(&self) -> u64 {
        let c = &self.state.cursor;
        let s = self.cursor_force_active.to_bits() as u64;
        (c.radius.to_bits() as u64)
            .wrapping_mul(31)
            .wrapping_add(c.strength.to_bits() as u64)
            .wrapping_mul(31)
            .wrapping_add(c.depth.to_bits() as u64)
            .wrapping_mul(31)
            .wrapping_add(s)
    }

    fn apply_cursor_force(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        // Release-edge detection: cursor was active last frame, now it's
        // not. Kick a short accelerated cool-down so the brief disturbance
        // halts before HALT_GRACE_STEPS (~5s at steps_per_call=2) elapses.
        if self.prev_cursor_force_active.abs() > 0.0
            && self.cursor_force_active.abs() == 0.0
        {
            self.post_click_cooldown_frames = 30;
            self.post_click_cooldown_applied = false;
        }
        self.prev_cursor_force_active = self.cursor_force_active;
        let key = self.cursor_key();
        if self.prev_cursor_key == Some(key) {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            if self.cursor_force_active.abs() > 0.0 {
                // Project the last canvas pointer to a world point at the
                // configured depth in front of the camera.
                let world: [f32; 3] = if let Some(pos) = self.last_pointer_in_canvas {
                    let rect = self.prev_canvas_rect.unwrap_or(egui::Rect::NOTHING);
                    let ndc_x = (pos.x - rect.left()) / rect.width().max(1.0) * 2.0 - 1.0;
                    let ndc_y = -((pos.y - rect.top()) / rect.height().max(1.0) * 2.0 - 1.0);
                    let (origin, dir) = pipes.camera.raycast(ndc_x, ndc_y);
                    let target = origin + dir * self.state.cursor.depth.max(1.0);
                    target.to_array()
                } else {
                    [0.0, 0.0, 0.0]
                };
                pipes.set_cursor_force(
                    world,
                    self.state.cursor.radius,
                    self.state.cursor.strength * self.cursor_force_active,
                );
            } else {
                pipes.set_cursor_force([0.0, 0.0, 0.0], 0.0, 0.0);
            }
        }
        self.prev_cursor_key = Some(key);
    }

    /// While the post-click cool-down window is active, push a temporary
    /// options snapshot with stronger cooling so the brief perturbation
    /// halts fast. When the window expires, clear `prev_layout_key` so
    /// the next `apply_layout_to_gpu` re-pushes the user's tuned values.
    ///
    /// We only push the perturbed opts on the *first* frame of the
    /// window. Re-pushing every frame would (a) compound the multiplications
    /// (`layout_options()` reads back the *current* opts, not the user's
    /// configured ones) and (b) re-trip `set_options`'s wake-gating each
    /// frame, defeating the cooldown. The post-cooldown
    /// `prev_layout_key = None` reset lets `apply_layout_to_gpu` repaint
    /// the user-configured opts in one shot.
    fn tick_post_click_cooldown(&mut self, frame: &mut eframe::Frame) {
        if self.post_click_cooldown_frames == 0 || !self.loaded_into_gpu {
            return;
        }
        self.post_click_cooldown_frames -= 1;
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            if !self.post_click_cooldown_applied {
                if let Some(mut opts) = pipes.layout_options() {
                    // Aggressive cooling tweaks — only for the cooldown window.
                    opts.cooling_alpha *= 0.95;
                    opts.energy_threshold *= 5.0;
                    pipes.update_layout_options(opts);
                    self.post_click_cooldown_applied = true;
                }
            }
        }
        if self.post_click_cooldown_frames == 0 {
            // Restore the user's tuned values via apply_layout_to_gpu's
            // normal path on the next frame.
            self.prev_layout_key = None;
            self.post_click_cooldown_applied = false;
        }
    }

    /// Hover throttle interval — ~50ms gives a comfortable 20 Hz max
    /// raycast cadence per the spec.
    const HOVER_THROTTLE_MS: u64 = 50;
    /// Delay between cursor-landed-on-node and the preview card opening.
    /// 700ms is short enough to feel responsive, long enough that
    /// sweeping across the canvas doesn't fire a card for every node.
    const HOVER_PREVIEW_DELAY_MS: u64 = 700;
    /// Hover-release hold — keep the previous hover focus engaged for
    /// this long after the cursor leaves a node, so a brief jitter or
    /// a quick gap between two nodes doesn't flash everything bright.
    const HOVER_HOLD_MS: u64 = 250;

    /// Throttled hover→focus pipeline. No-op while sticky-focus is set
    /// (per spec: don't fight the user). Drives `focus_hover_idx`.
    fn update_hover_focus(
        &mut self,
        frame: &mut eframe::Frame,
        pointer_in_canvas: Option<egui::Pos2>,
        canvas_rect: Option<egui::Rect>,
    ) {
        // Sticky wins; a sticky-focus user gesture overrides hover.
        if self.focus_sticky_idx.is_some() {
            self.focus_hover_idx = None;
            self.focus_hover_edge_idx = None;
            self.hover_clear_at = None;
            return;
        }
        // No focus mode work to do if there's no canvas to hover over.
        let (Some(rect), Some(pos)) = (canvas_rect, pointer_in_canvas) else {
            self.focus_hover_edge_idx = None;
            self.maybe_clear_hover_after_hold();
            return;
        };
        let now = web_time::Instant::now();
        let throttled = self
            .last_hover_raycast_at
            .map(|t| (now - t).as_millis() < Self::HOVER_THROTTLE_MS as u128)
            .unwrap_or(false);
        if throttled {
            return;
        }
        self.last_hover_raycast_at = Some(now);

        let rect_w = rect.width().max(1.0);
        let rect_h = rect.height().max(1.0);
        let ndc_x = (pos.x - rect.left()) / rect_w * 2.0 - 1.0;
        let ndc_y = -((pos.y - rect.top()) / rect_h * 2.0 - 1.0);
        let raw_hit = self.raycast_idx(frame, ndc_x, ndc_y, [rect_w, rect_h]);
        // Filter-out gate: when the user has the filter behavior set to
        // `Filter` (discard non-matches) AND a non-empty filter set is
        // active, raycast hits on filtered-out nodes must be ignored —
        // otherwise the hover-preview panel + its tether line render
        // for an invisible node, making the filtered node visually
        // reappear (via the panel chrome, not the wgpu shader — the
        // shader correctly culls). For `FilterBehavior::Focus`
        // (non-matches dimmed but visible) filtered nodes stay
        // hoverable — that's the whole point of focus mode.
        let hit = match raw_hit {
            Some(idx)
                if matches!(
                    self.state.filter_behavior,
                    crate::ui::state::FilterBehavior::Filter
                ) =>
            {
                let allowed = self
                    .field_index
                    .as_ref()
                    .and_then(|fi| fi.matches(&self.state.query.active_filters))
                    .map(|set| set.contains(&idx))
                    .unwrap_or(true);
                if allowed { Some(idx) } else { None }
            }
            other => other,
        };
        match hit {
            Some(idx) => {
                if self.focus_hover_idx != Some(idx) {
                    self.focus_hover_idx = Some(idx);
                }
                // Node hover takes priority over edge hover.
                self.focus_hover_edge_idx = None;
                // Hovering — cancel any pending clear timer.
                self.hover_clear_at = None;
            }
            None => {
                // Fall back to edge picking — only highlights when no
                // node is under the cursor.
                let edge_hit = self.raycast_edge_idx(frame, ndc_x, ndc_y, [rect_w, rect_h]);
                self.focus_hover_edge_idx = edge_hit;
                self.maybe_clear_hover_after_hold();
            }
        }
    }

    /// Mirrors `raycast_idx` but defers to `GraphPipelines::raycast_edge`.
    /// The effective edge width is derived from the user's tuned style
    /// fields so the pick tolerance tracks the visual edge width.
    fn raycast_edge_idx(
        &self,
        frame: &eframe::Frame,
        ndc_x: f32,
        ndc_y: f32,
        screen_px: [f32; 2],
    ) -> Option<u32> {
        let wgpu_state = frame.wgpu_render_state()?;
        let renderer = wgpu_state.renderer.read();
        let pipes = renderer.callback_resources.get::<GraphPipelines>()?;
        // Mirror the same width feed `apply_layout_to_gpu` pushes via
        // `set_edge_style`, so the pick band tracks the visual edge
        // width the user sees on screen.
        let s = &self.state.style;
        let edge_w = (s.edge_width
            * apply_size_scale(s.edge_size_mul, s.log_scale_size))
            .max(0.0);
        pipes.raycast_edge([ndc_x, ndc_y], screen_px, edge_w)
    }

    /// Drive the hover-preview state machine. Called each frame after
    /// `update_hover_focus`. Three phases:
    ///   1. Cursor lands on a new node → arm the delay timer; cancel
    ///      any open card.
    ///   2. Delay elapses while cursor is still on the same node →
    ///      kick off `/node/:id` fetch and flip the card open. Cached
    ///      meta from a prior fetch shortcuts the network hop.
    ///   3. Cursor leaves the node OR moves to a different one → clear
    ///      the card; the next landing arms a fresh timer.
    fn tick_hover_preview(&mut self, pointer_in_canvas: Option<egui::Pos2>) {
        // Sticky-focus mode (click selection) suppresses preview — the
        // inspector / modal already cover that node's detail surface.
        if self.focus_sticky_idx.is_some() {
            self.close_hover_preview();
            return;
        }
        let now = web_time::Instant::now();
        match self.focus_hover_idx {
            Some(idx) => {
                // Transition: hover landed on a different node (or
                // first-time landing). Reset arm timer; close any old
                // card. The cached meta stays in case the user
                // re-hovers the SAME id later — no fetch on re-entry.
                if self.hover_preview_idx != Some(idx) {
                    self.hover_preview_idx = Some(idx);
                    self.hover_preview_armed_at = Some(now);
                    self.hover_preview_open = false;
                    self.hover_preview_pos = pointer_in_canvas;
                    // If the cached meta is for this idx already, the
                    // open path below will re-show it without a refetch.
                    let want_id = self.id_for_idx(idx);
                    let cached_for_same =
                        self.hover_preview_meta.as_ref().and_then(|m| {
                            want_id.as_deref().map(|wid| m.id == wid)
                        }).unwrap_or(false);
                    if !cached_for_same {
                        self.hover_preview_meta = None;
                    }
                }
                // Latch position from the last canvas-pointer reading
                // so we don't anchor at the wrong spot if the pointer
                // is None this frame (egui mid-drag etc).
                if let Some(p) = pointer_in_canvas {
                    self.hover_preview_pos = Some(p);
                }
                // Check delay → open path.
                let armed_long_enough = self
                    .hover_preview_armed_at
                    .map(|t| (now - t).as_millis() >= Self::HOVER_PREVIEW_DELAY_MS as u128)
                    .unwrap_or(false);
                if armed_long_enough && !self.hover_preview_open {
                    self.hover_preview_open = true;
                    // Kick a fetch only if we don't already have meta
                    // for this id. The result drains into modal/main
                    // path lazily; we also poll the fetch slot below.
                    let Some(id) = self.id_for_idx(idx) else { return };
                    let has_cached = self
                        .hover_preview_meta
                        .as_ref()
                        .map(|m| m.id == id)
                        .unwrap_or(false);
                    if !has_cached {
                        let slot = self.hover_preview_fetch.clone();
                        let client = ApiClient::new(self.base_url.clone());
                        let sink = self.progress.sink();
                        let label = format!("preview {}", short_id(&id));
                        spawn_async(async move {
                            let task = sink.start("hover", label);
                            let res = client.node(&id).await;
                            match &res {
                                Ok(_) => sink.finish(task),
                                Err(e) => sink.fail(task, e.clone()),
                            }
                            *slot.lock().unwrap() = Some(res);
                        });
                    }
                }
            }
            None => self.close_hover_preview(),
        }

        // Drain any completed hover fetch into hover_preview_meta.
        let drained = self.hover_preview_fetch.lock().unwrap().take();
        if let Some(Ok(Some(meta))) = drained {
            self.hover_preview_meta = Some(meta);
        }
    }

    fn close_hover_preview(&mut self) {
        self.hover_preview_idx = None;
        self.hover_preview_armed_at = None;
        self.hover_preview_open = false;
        self.hover_preview_pos = None;
    }

    /// Drain async slots backing the promoted-node panel: the
    /// `/node/:id` meta fetch and any completed `/vault/page` save. Run
    /// before the panel paints so a freshly-arrived NodeMeta / saved body
    /// shows the same frame. (Previously inlined at the top of
    /// `show_hover_preview`, which ran after the Node panel rendered.)
    fn drain_node_fetches(&mut self) {
        if let Some(Ok(Some(meta))) =
            self.promoted_anchored_fetch.lock().unwrap().take()
        {
            self.promoted_anchored_meta = Some(meta);
        }

        if let Some((node_id, body, result)) =
            self.save_in_flight.lock().unwrap().take()
        {
            if let Some(state) = self.page_viewer_states.get_mut(&node_id) {
                match &result {
                    Ok(()) => state.note_saved(body.clone()),
                    Err(e) => state.note_save_error(e.clone()),
                }
            }
            if result.is_ok() {
                if let Some(meta) = self.promoted_anchored_meta.as_mut() {
                    if meta.id == node_id {
                        meta.body = body;
                    }
                }
            }
        }
    }

    /// Draw the hover-preview card if armed.
    ///
    /// Anchored to the hovered node's projected screen position (not
    /// the cursor) via [`ui::anchored::AnchoredPanel`], so the card
    /// tracks the node as the camera pans / zooms / orbits and the
    /// tether arrow visibly ties the card back to its source. A
    /// 1-frame EMA (`EMA_ALPHA = 0.4`) on the projected screen
    /// position smooths out force-sim jitter — without it the card
    /// vibrates every frame the GPU position readback shifts by a
    /// sub-pixel.
    ///
    /// The card is `interactable(true)` (vs. the previous
    /// cursor-anchored implementation): promoted click-to-pin will
    /// reuse the same path, and a scrollable markdown body needs
    /// interactivity. The cursor-leaves-card flicker the old
    /// comment warned about doesn't happen here because the anchor
    /// is the node, not the cursor — the user can move the cursor
    /// freely between node and card.
    fn show_hover_preview(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        let Some(canvas_rect) = self.prev_canvas_rect else {
            return;
        };

        // The promoted node no longer renders here — it is a unified
        // `FloatingPanel` (or a workspace tile), rendered by
        // `render_node_panel_floating` / the Node tile so it shares the
        // exact panel component + float/tile traffic-light chrome as
        // Layout/Filters/Camera. The
        // only anchored card left in this pass is the transient HOVER
        // preview tether, which is a legitimately world-anchored
        // affordance. Skip it when it would double the promoted node.
        let hover_idx = if self.hover_preview_open {
            self.hover_preview_idx
        } else {
            None
        };
        let promoted_idx = self.promoted_anchored_idx;

        if let Some(hidx) = hover_idx {
            if Some(hidx) != promoted_idx {
                if let Some(meta) = self.hover_preview_meta.clone() {
                    self.render_anchored_panel(
                        ctx, frame, canvas_rect, hidx, meta,
                    );
                }
            }
        }
    }

    /// Render the transient HOVER preview card for `idx`.
    ///
    /// This is the world-anchored tether affordance only — the promoted /
    /// expanded node now renders as a `FloatingPanel` via
    /// [`Self::render_node_panel_floating`]. The hover card uses the EMA-smoothed
    /// positioning + tether pipeline: project once, blend into
    /// `last_anchored_screen_pos`, hand the smoothed value to
    /// AnchoredPanel as `screen_pos_override`.
    fn render_anchored_panel(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        canvas_rect: egui::Rect,
        idx: u32,
        meta: proto::NodeMeta,
    ) {
        // Hover preview is always compact (no expand/maximize, no window
        // chrome, no channels) — those moved to the FloatingPanel-based
        // Node panel. This path is purely the world-anchored tether card.
        // Pull world position + camera snapshot out of the wgpu
        // callback resources, then drop the read lock before opening
        // any egui Area. Camera is cloned (small, all-Copy fields)
        // so AnchoredPanel can borrow without holding the wgpu lock
        // across egui calls.
        let (world, camera) = {
            let Some(wgpu_state) = frame.wgpu_render_state() else {
                return;
            };
            let renderer = wgpu_state.renderer.read();
            let Some(pipes) = renderer.callback_resources.get::<GraphPipelines>() else {
                return;
            };
            let positions = pipes.positions_cpu();
            let i3 = (idx as usize).saturating_mul(3);
            if i3 + 2 >= positions.len() {
                return;
            }
            let world = glam::Vec3::new(positions[i3], positions[i3 + 1], positions[i3 + 2]);
            (world, pipes.camera.clone())
        };

        // Project + EMA-smooth. EMA_ALPHA = 0.4: blend 40% of the
        // new frame into the running average. Below 0.4 the card
        // visibly lags fast camera pans; above 0.6 the jitter
        // reappears. We re-checked 0.4 after wiring the smoothed
        // value through `screen_pos_override` (so the user actually
        // sees the smoothed position drive placement, not the
        // projected one); 0.4 still reads as crisp without jitter.
        const EMA_ALPHA: f32 = 0.4;
        let smoothed: Option<egui::Pos2> = {
            let aspect = (canvas_rect.width() / canvas_rect.height().max(0.0001)).max(0.0001);
            let view = glam::Mat4::look_to_rh(camera.position, camera.forward(), glam::Vec3::Y);
            let proj = glam::Mat4::perspective_rh(
                camera.fov_y,
                aspect,
                camera.znear,
                camera.zfar,
            );
            let clip = (proj * view) * world.extend(1.0);
            if clip.w > 0.0 {
                let ndc_x = clip.x / clip.w;
                let ndc_y = clip.y / clip.w;
                let sx = canvas_rect.left() + (ndc_x * 0.5 + 0.5) * canvas_rect.width();
                let sy = canvas_rect.top()
                    + (1.0 - (ndc_y * 0.5 + 0.5)) * canvas_rect.height();
                let projected = egui::pos2(sx, sy);
                let s = match self.last_anchored_screen_pos.get(&idx).copied() {
                    Some(p) => egui::pos2(
                        p.x + (projected.x - p.x) * EMA_ALPHA,
                        p.y + (projected.y - p.y) * EMA_ALPHA,
                    ),
                    None => projected,
                };
                self.last_anchored_screen_pos.insert(idx, s);
                Some(s)
            } else {
                None
            }
        };

        let panel = crate::ui::anchored::AnchoredPanel::new(
            egui::Id::new(("hover-preview", idx)),
            world,
            canvas_rect,
            &camera,
        )
        .offset(egui::vec2(18.0, 18.0))
        .interactable(true)
        .screen_pos_override(smoothed)
        // 360 wide × generous height estimate so hover previews anchored
        // near the bottom don't overflow. Tracks `set_max_width(360.0)`.
        .reserved_size(egui::vec2(360.0, 240.0));

        let _output = panel.show(ctx, |ui| {
            ui.set_max_width(360.0);

            // Lightweight, non-interactive header: drag glyph + title.
            // The hover preview vanishes on cursor-leave, so it carries
            // no window chrome (that lives on the promoted FloatingPanel).
            ui.horizontal(|ui| {
                let title = if !meta.title.is_empty() {
                    meta.title.clone()
                } else if !meta.path.is_empty() {
                    std::path::Path::new(&meta.path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| meta.id.clone())
                } else if !meta.id.is_empty() {
                    meta.id.clone()
                } else {
                    "Node".to_string()
                };
                ui.label(
                    egui::RichText::new("\u{2630}")
                        .small()
                        .color(crate::ui::theme::palette::ICON),
                );
                ui.label(
                    egui::RichText::new(&title)
                        .strong()
                        .color(crate::ui::theme::palette::TEXT),
                );
            });

            if !meta.path.is_empty() {
                ui.label(
                    egui::RichText::new(&meta.path)
                        .small()
                        .weak()
                        .monospace(),
                );
            }
            if !meta.tags.is_empty() {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(meta.tags.join(", "))
                        .small()
                        .color(crate::ui::theme::palette::INFO),
                );
            }
            if !meta.body.is_empty() {
                ui.separator();
                let snippet = body_snippet(&meta.body, 280, 6);
                ui.label(
                    egui::RichText::new(snippet)
                        .small()
                        .color(crate::ui::theme::palette::TEXT),
                );
            }

            // The hover preview has no draggable header; hand back a
            // non-draggable dummy response (AnchoredPanel only reads
            // drag/double-click from it, which the transient card ignores).
            let dummy = ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover());
            ((), dummy)
        });
    }

    /// Render the promoted-node ("Node") panel in its FLOATING form, via
    /// the SHARED [`crate::ui::floating::FloatingPanel`] component.
    ///
    /// This is the unified replacement for the old expanded/maximized
    /// AnchoredPanel path. The promoted node gets the same macOS
    /// traffic-light chrome as every other panel: red close, yellow
    /// collapse, and the GREEN Float⇄Tile toggle (`with_placement`). The
    /// body is `inspector::render_body` (via `render_node_body`). The
    /// TILED form is rendered by the workspace SidePanel as a
    /// `PaneKind::Node` tile (the `node_body` closure handed to the
    /// `show_workspace_panel` call in `update`). Exactly one form renders
    /// per frame — floating XOR tiled
    /// — and the hover preview is skipped when its idx matches the
    /// promoted idx, so the old "compact card behind the expanded card"
    /// overlap is structurally impossible.
    ///
    /// No-op unless a node is promoted AND its placement is Floating.
    fn render_node_panel_floating(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        channels: &mut AnchoredChannels,
    ) {
        let Some(idx) = self.promoted_anchored_idx else {
            return;
        };
        if self.state.node_panel_placement != crate::ui::tiles::Placement::Floating {
            return; // tiled form renders in the workspace
        }
        let Some(meta) = self.promoted_anchored_meta.clone() else {
            return;
        };

        let edges_snapshot = self.edges_snapshot(frame);
        let placement_before = self.state.node_panel_placement;

        let active_filters_snapshot = self.state.query.active_filters.clone();
        let color_by = self.state.style.color_by;
        let palette = self.state.style.palette;
        let mut tag_query = self.state.tag_browser_query.clone();
        let title = node_title(&meta);

        let panel_id = crate::ui::state::PanelId::Node;
        let mut open = true;
        let mut placement = placement_before;
        let mut collapsed =
            self.node_panel_collapsed.get(&idx).copied().unwrap_or(false);
        let mut focused = std::mem::take(&mut self.state.focused_panel);
        let my_focus = crate::ui::state::FocusedPanel::AnchoredNode(idx);

        // Destructure disjoint App fields so the body closure borrows only
        // what it needs (not `self`).
        let App {
            ids,
            metrics,
            field_index,
            page_viewer_states,
            page_viewer_markdown_cache,
            ..
        } = self;
        let ids = &*ids;
        let metrics = &*metrics;
        let field_index = field_index.as_ref();
        let meta_ref = &meta;
        let edges_ref = &edges_snapshot;

        // The panel chrome title IS the node title, so the title renders
        // exactly once. (Previously the chrome showed a literal "Node" AND
        // the body re-drew `title`, reading as two stacked titles.)
        crate::ui::floating::FloatingPanel::new(panel_id, title)
            .default_pos([320.0, 96.0])
            .default_size([480.0, 600.0])
            .with_placement(&mut placement)
            .with_focus(&mut focused, my_focus)
            .with_collapsed(&mut collapsed)
            .show(ctx, &mut open, |ui| {
                ui.set_max_width(460.0);
                // The FloatingPanel lives in an auto-sizing window; give the
                // inner ScrollArea an explicit budget so it clips+scrolls.
                let avail = ui.available_height();
                let max_h = if avail.is_finite() && avail > 4.0 && avail < 4000.0 {
                    (avail - 8.0).max(160.0)
                } else {
                    520.0
                };
                render_node_body(
                    ui, max_h, idx, meta_ref, ids, metrics, edges_ref, color_by,
                    palette, &active_filters_snapshot, field_index,
                    page_viewer_states, page_viewer_markdown_cache, &mut tag_query,
                    channels,
                );
            });

        self.state.focused_panel = focused;
        self.state.tag_browser_query = tag_query;
        if collapsed {
            self.node_panel_collapsed.insert(idx, true);
        } else {
            self.node_panel_collapsed.remove(&idx);
        }
        // Green-dot toggle flipped placement → snap into the tree so the
        // workspace renders the tile next frame.
        if placement != placement_before {
            self.state.node_panel_placement = placement;
            if placement == crate::ui::tiles::Placement::Tiled {
                let mut ws = std::mem::take(&mut self.state.tiles);
                ws.snap_insert(crate::ui::tiles::PaneKind::Node);
                self.state.tiles = ws;
            }
        }
        // Red close → dismiss the promoted node entirely.
        if !open {
            self.dismiss_promoted_node(idx);
        }
    }

    /// Read-only mirror of the GPU edge buffer (packed [src, tgt, ...]).
    fn edges_snapshot(&self, frame: &eframe::Frame) -> Vec<u32> {
        if let Some(wgpu_state) = frame.wgpu_render_state() {
            let renderer = wgpu_state.renderer.read();
            renderer
                .callback_resources
                .get::<GraphPipelines>()
                .map(|p| p.edges_cpu().to_vec())
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Clear the promoted node + its focus (shared by the floating close
    /// button and the tiled-tile close).
    fn dismiss_promoted_node(&mut self, idx: u32) {
        self.promoted_anchored_idx = None;
        self.promoted_anchored_meta = None;
        self.state.node_panel_open = false;
        if matches!(
            self.state.focused_panel,
            Some(crate::ui::state::FocusedPanel::AnchoredNode(i)) if i == idx
        ) {
            self.state.focused_panel = None;
        }
    }

    fn maybe_clear_hover_after_hold(&mut self) {
        if self.focus_hover_idx.is_none() {
            self.hover_clear_at = None;
            return;
        }
        let now = web_time::Instant::now();
        match self.hover_clear_at {
            None => {
                self.hover_clear_at = Some(now);
            }
            Some(t) if (now - t).as_millis() >= Self::HOVER_HOLD_MS as u128 => {
                self.focus_hover_idx = None;
                self.hover_clear_at = None;
            }
            _ => {}
        }
    }

    /// Auto-flip the active [`FocusMode`] to [`FocusMode::Filter`] on
    /// the empty→non-empty `active_filters` edge, and restore the saved
    /// mode on the non-empty→empty edge.
    ///
    /// Conservative rule: we only snapshot at the empty→non-empty edge
    /// and only restore at the non-empty→empty edge. Any manual mode
    /// change made *while* filters are active is preserved (we never
    /// re-flip mid-session), and the snapshot is session-only — an app
    /// reload starts with `previous_focus_mode = None`, so a persisted
    /// non-empty filter set produces no phantom restore on the first
    /// non-empty→empty transition either (we'd just have nothing to
    /// restore).
    fn handle_filter_focus_auto_flip(&mut self) {
        let now_non_empty =
            !self.state.query.active_filters.by_field.is_empty();
        match (self.prev_filters_non_empty, now_non_empty) {
            (false, true) => {
                let prev = self.state.focus.focus_mode;
                self.previous_focus_mode = Some(prev);
                if prev != FocusMode::Filter {
                    self.state.focus.focus_mode = FocusMode::Filter;
                    log::info!(
                        "[graph-renderer] focus auto-flipped: prev={:?} -> Filter",
                        prev
                    );
                }
            }
            (true, false) => {
                if let Some(prev) = self.previous_focus_mode.take() {
                    self.state.focus.focus_mode = prev;
                    log::info!(
                        "[graph-renderer] focus restored: -> {:?}",
                        prev
                    );
                }
            }
            _ => {}
        }
        self.prev_filters_non_empty = now_non_empty;
    }

    /// Resolve the active focused-node idx (sticky beats hover) and push
    /// the per-node `dim_alpha` mask to the GPU when it changes.
    ///
    /// Coexists with the QueryModel selection path
    /// (`apply_selection`/`set_selected`): both write per-node alpha,
    /// but through *separate* storage buffers — `colors` (selection-side)
    /// and `dim_alpha` (focus-side). The node shader multiplies them, so
    /// a focused node inside the selection set stays bright; out-of-set,
    /// out-of-focus nodes drop multiplicatively dimmer. Documented at
    /// the call site (`apply_focus_set_to_gpu` follows `apply_selection`
    /// on purpose).
    fn apply_focus_set_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let focused = self.focus_sticky_idx.or(self.focus_hover_idx);
        let mode = self.state.focus.focus_mode;
        // Stable signature of the current active-filter set so a chip
        // toggle re-runs this function even when `focused` is unchanged.
        // BTreeMap iteration is deterministic, so the hash is stable.
        let filter_sig: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            for (k, vs) in self.state.query.active_filters.by_field.iter() {
                k.hash(&mut h);
                for v in vs.iter() { v.hash(&mut h); }
                // Per-field combinator participates so toggling it
                // re-runs the GPU push.
                (self.state.query.active_filters.combinator_for(k) as u8).hash(&mut h);
            }
            (self.state.query.active_filters.cross_field_combinator as u8).hash(&mut h);
            // FilterBehavior changes which GPU path we take — include
            // it so flipping the toggle forces a re-write.
            (self.state.filter_behavior as u8).hash(&mut h);
            h.finish()
        };
        // Change-detect to avoid pointless GPU writes (dim_alpha = n_nodes
        // f32s — cheap, but worth skipping on idle frames).
        if self.focus_pushed_idx == focused
            && self.focus_pushed_mode == Some(mode)
            && self.filter_pushed_sig == Some(filter_sig)
        {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let queue = wgpu_state.queue.clone();
        match focused {
            Some(idx) => {
                // Node-focus path: compute the community membership and
                // write it via `set_focus_set` (non-zero dims, keeps the
                // graph visible but faded).
                let edges_snapshot: Vec<u32> = {
                    let renderer = wgpu_state.renderer.read();
                    renderer
                        .callback_resources
                        .get::<GraphPipelines>()
                        .map(|p| p.edges_cpu().to_vec())
                        .unwrap_or_default()
                };
                let n_nodes = self.ids.len() as u32;
                let ctx = FocusCtx {
                    n_nodes,
                    metrics: &self.metrics,
                    edges: &edges_snapshot,
                    node_meta: None,
                    query: Some(&self.state.query),
                    field_index: self.field_index.as_ref(),
                };
                let members = focus_set::compute(idx, mode, &ctx);
                let mut renderer = wgpu_state.renderer.write();
                if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
                    pipes.set_focus_set(&queue, focused, &members);
                }
                log::info!("[graph-renderer] focus: members={}", members.len());
            }
            None => {
                // No node-focus. If filters are active, dispatch on
                // `state.filter_behavior`:
                //   - `Filter`: discard non-matches via `set_filter_mask`.
                //   - `Focus`:  dim non-matches via `set_focus_set` (the
                //               pre-ca7d40d7 behavior).
                // Always clear the *other* path so toggling between
                // modes doesn't leave stale state on the GPU.
                let matching: Option<HashSet<u32>> = self
                    .field_index
                    .as_ref()
                    .and_then(|fi| fi.matches(&self.state.query.active_filters));
                let behavior = self.state.filter_behavior;
                let mut renderer = wgpu_state.renderer.write();
                if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
                    match behavior {
                        crate::ui::state::FilterBehavior::Filter => {
                            // Reset the focus dim mask, then push the
                            // hard filter mask.
                            pipes.set_focus_set(&queue, None, &HashSet::new());
                            pipes.set_filter_mask(&queue, matching.as_ref());
                        }
                        crate::ui::state::FilterBehavior::Focus => {
                            // Clear the hard filter mask, then dim via
                            // focus_set. Empty matching → no dim (all
                            // visible).
                            pipes.set_filter_mask(&queue, None);
                            let members: HashSet<u32> = matching.unwrap_or_default();
                            pipes.set_focus_set(&queue, None, &members);
                        }
                    }
                }
            }
        }
        self.focus_pushed_idx = focused;
        self.focus_pushed_mode = Some(mode);
        self.filter_pushed_sig = Some(filter_sig);
    }

    /// Push the current `focus_hover_idx` to the shader's hover-glow
    /// uniform. Change-detected against `hovered_pushed_idx` so we
    /// don't write the effects uniform when the cursor sits still over
    /// the same node.
    fn apply_hover_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu { return; }
        let target = self.focus_hover_idx;
        if self.hovered_pushed_idx == target {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            pipes.set_hovered_node(target);
        }
        self.hovered_pushed_idx = target;
    }

    /// Push the current `focus_hover_edge_idx` to the shader's edge
    /// hover uniform. Change-detected against `hovered_pushed_edge_idx`
    /// so we don't write the effects uniform on every idle frame.
    fn apply_edge_hover_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu { return; }
        let target = self.focus_hover_edge_idx;
        if self.hovered_pushed_edge_idx == target {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            pipes.set_hovered_edge(target);
        }
        self.hovered_pushed_edge_idx = target;
    }

    fn apply_selection(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let cache = self.search_cache.lock().unwrap().clone();
        let ctx = EvalContext::new(&self.ids, &self.id_to_idx, &cache)
            .with_field_index(self.field_index.as_ref());
        let selected = self.state.query.evaluate(&ctx);
        // hash for change-detect
        let h: u64 = match &selected {
            None => 0,
            Some(set) => {
                let mut acc: u64 = 1;
                for &i in set {
                    acc = acc.wrapping_mul(0x100_0000_01b3) ^ i as u64;
                }
                acc
            }
        };
        if self.prev_selected_hash == Some(h) {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let queue = wgpu_state.queue.clone();
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            pipes.set_selected(&queue, selected.as_ref());
        }
        self.prev_selected_hash = Some(h);
    }

    fn refresh_stats(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let renderer = wgpu_state.renderer.read();
        let mut sim_halted = false;
        if let Some(pipes) = renderer.callback_resources.get::<GraphPipelines>() {
            self.state.stats.n_nodes = pipes.n_nodes();
            self.state.stats.n_edges = pipes.n_edges();
            sim_halted = pipes.is_halted();
            // Mirror max-KE so the renderer can pick repaint cadence
            // without re-locking the renderer in the update loop tail.
            self.last_observed_max_ke = pipes.last_max_ke();
        }
        // Communities: max value + 1 over the community metric.
        if let Some(comm) = self.metrics.get("community") {
            let mut mx: i64 = -1;
            for &v in comm {
                let i = v as i64;
                if i > mx {
                    mx = i;
                }
            }
            self.state.stats.n_communities = (mx + 1).max(0) as u32;
        }
        // Sim status reflects the real GPU-force halt state. Once
        // max-KE has stayed under `energy_threshold` for `HALT_FRAMES`
        // consecutive readbacks, GraphPipelines::is_halted flips true and
        // we surface "settled" in the Stats panel.
        self.state.sim_status = if sim_halted {
            ui::state::SimStatus::Settled
        } else {
            ui::state::SimStatus::Running
        };

        // Mirror live ActionInstances back into AppState so the next
        // eframe::Storage::save catches them.
        if self.state.action_instances.len() != self.actions.instances.len()
            || self.state.action_instances != self.actions.instances
        {
            self.state.action_instances = self.actions.instances.clone();
        }
    }

    /// Dispatch a built-in action variant. Mutates AppState (and the wgpu
    /// graph layer where applicable), then records an `ActionInstance`.
    fn execute_action(
        &mut self,
        frame: &mut eframe::Frame,
        action_id: &str,
        params: HashMap<String, ParamValue>,
    ) {
        let Some(action) = self.actions.get(action_id).cloned() else { return };
        let ActionHandlerVariant::Builtin(variant) = handler_variant(&action);
        // Parent-only actions in the palette tree drill into children;
        // they should not produce instances even if accidentally executed.
        if !action.children_ids.is_empty() && action.parameters.is_empty() {
            return;
        }
        let result = self.run_builtin(frame, variant, &params);
        self.actions.record_execution(&action.id, params, result);
    }

    fn run_builtin(
        &mut self,
        frame: &mut eframe::Frame,
        variant: BuiltinAction,
        params: &HashMap<String, ParamValue>,
    ) -> serde_json::Value {
        use crate::ui::query::Card;
        use BuiltinAction::*;
        match variant {
            Settings | NodeOperations | Filter => serde_json::json!({}),

            EditOptions => {
                if let Some(ParamValue::Number(n)) = params.get("font_size") {
                    self.state.workspace.font_size = (*n as f32).clamp(8.0, 32.0);
                }
                if let Some(ParamValue::Selected(items)) = params.get("font_family") {
                    if let Some(v) = items.first() {
                        self.state.workspace.font_family = parse_font_family(v);
                    }
                }
                if let Some(ParamValue::Boolean(b)) = params.get("show_line_numbers") {
                    self.state.workspace.show_line_numbers = *b;
                }
                serde_json::json!({ "settings": workspace_json(&self.state.workspace) })
            }
            FontSize => {
                if let Some(ParamValue::Number(n)) = params.get("font_size") {
                    self.state.workspace.font_size = (*n as f32).clamp(8.0, 32.0);
                }
                serde_json::json!({ "font_size": self.state.workspace.font_size })
            }
            FontFamily => {
                if let Some(ParamValue::Selected(items)) = params.get("font_family") {
                    if let Some(v) = items.first() {
                        self.state.workspace.font_family = parse_font_family(v);
                    }
                }
                serde_json::json!({ "font_family": format!("{:?}", self.state.workspace.font_family) })
            }
            LineNumbers => {
                if let Some(ParamValue::Boolean(b)) = params.get("show_line_numbers") {
                    self.state.workspace.show_line_numbers = *b;
                }
                serde_json::json!({ "show_line_numbers": self.state.workspace.show_line_numbers })
            }
            ToggleTheme => {
                // The renderer is dark-mode only; this records intent but
                // doesn't flip the theme until a light variant exists.
                serde_json::json!({ "theme": "dark" })
            }

            FilterByName | FilterByContent => {
                let field = if matches!(variant, FilterByName) { "name" } else { "content" };
                let pattern = params
                    .get("pattern")
                    .and_then(|v| v.as_string())
                    .unwrap_or("")
                    .to_string();
                self.append_filter_card(field.to_string(), pattern.clone());
                serde_json::json!({ "filter": { "type": field, "pattern": pattern } })
            }
            FilterByTag => {
                let tags: Vec<String> = params
                    .get("tags")
                    .and_then(|v| v.as_selected())
                    .unwrap_or(&[])
                    .to_vec();
                for t in &tags {
                    self.append_filter_card("tag".into(), t.clone());
                }
                serde_json::json!({ "filter": { "type": "tag", "tags": tags } })
            }
            SearchNodes => {
                let q = params
                    .get("query")
                    .and_then(|v| v.as_string())
                    .unwrap_or("")
                    .to_string();
                if !q.is_empty() {
                    self.state.query.cards.push(Card::Search { value: q.clone(), regex: false });
                }
                serde_json::json!({ "search": { "query": q } })
            }
            CreateNode => {
                // Node creation against a server-loaded vault is a no-op
                // here — the server owns the vault. Recorded for parity.
                let name = params
                    .get("name")
                    .and_then(|v| v.as_string())
                    .unwrap_or("New Node")
                    .to_string();
                serde_json::json!({ "node": { "name": name } })
            }

            FitCamera => {
                if let Some(wgpu_state) = frame.wgpu_render_state() {
                    let mut renderer = wgpu_state.renderer.write();
                    if let Some(pipes) =
                        renderer.callback_resources.get_mut::<GraphPipelines>()
                    {
                        pipes.fit_camera();
                    }
                }
                serde_json::json!({ "fit": true })
            }
            ResetStyle => {
                self.state.style = Default::default();
                self.prev_style_key = None;
                serde_json::json!({ "style": "reset" })
            }
            JumpToSection(sec) => {
                self.state.set_section_open(sec, true);
                serde_json::json!({ "section": sec.title() })
            }
            NewGraphTab => {
                self.state.dock.push_tab(crate::ui::workspace::TabKind::Graph);
                serde_json::json!({ "tab": "graph" })
            }
        }
    }
}

/// Per-frame request channels populated by the promoted-node panel's
/// inspector body (`inspector::render_body`), in either its floating
/// (`render_node_panel_floating`) or tiled (`PaneKind::Node`) form.
/// `App::update` declares one of these on the stack each frame and drains
/// it near the end of `update`.
///
/// Why a struct: `render_node_body` mounts the inspector body, which needs
/// the same outgoing channels the old free-standing inspector used
/// (`requested_selection`, `requested_filter_toggle`, navigate, url,
/// focus-node, page-save). Bundling them keeps the signatures short and
/// the drain block symmetrical with what `inspector::show_floating` used
/// to write.
// `pub` so the regression test crate can construct one (via `Default`)
// and drive the real `render_node_body` rather than a hand-copied mirror
// that could drift from production. Doc-hidden: not part of the stable
// public API surface, only the test harness reaches for it.
#[doc(hidden)]
#[derive(Default)]
pub struct AnchoredChannels {
    requested_selection: Option<u32>,
    requested_filter_toggle: Option<(String, String)>,
    requested_navigate: Option<String>,
    requested_open_url: Option<String>,
    requested_focus_node: Option<String>,
    requested_page_save: Option<(String, String, String)>,
}

/// Tiny indirection so the match in `execute_action` doesn't need to
/// borrow through `Action`'s `handler` field while we still hold the
/// cloned action.
enum ActionHandlerVariant {
    Builtin(BuiltinAction),
}

fn handler_variant(action: &actions::Action) -> ActionHandlerVariant {
    match &action.handler {
        actions::ActionHandler::Builtin(b) => ActionHandlerVariant::Builtin(b.clone()),
    }
}

fn parse_font_family(s: &str) -> FontFamilyChoice {
    match s {
        "monospace" => FontFamilyChoice::Monospace,
        "sans-serif" => FontFamilyChoice::SansSerif,
        "serif" => FontFamilyChoice::Serif,
        _ => FontFamilyChoice::Monospace,
    }
}

fn workspace_json(ws: &ui::state::WorkspaceSettings) -> serde_json::Value {
    serde_json::json!({
        "font_size": ws.font_size,
        "font_family": format!("{:?}", ws.font_family),
        "show_line_numbers": ws.show_line_numbers,
    })
}

fn default_base_url() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(origin) = window.location().origin() {
                return origin;
            }
        }
        "".to_string()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("GRAPH_API_URL").unwrap_or_else(|_| "http://127.0.0.1:4848".into())
    }
}

/// Display title for a node: `meta.title` → filename stem → `meta.id` →
/// "Node".
fn node_title(meta: &proto::NodeMeta) -> String {
    if !meta.title.is_empty() {
        meta.title.clone()
    } else if !meta.path.is_empty() {
        std::path::Path::new(&meta.path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| meta.id.clone())
    } else if !meta.id.is_empty() {
        meta.id.clone()
    } else {
        "Node".to_string()
    }
}

/// Shared body of the promoted-node panel: path + tags header, then the
/// full inspector body (`inspector::render_body`). Used by BOTH the
/// floating `FloatingPanel` and the tiled `PaneKind::Node` so the two
/// placements render identically. `max_h` bounds the inner ScrollArea.
// `pub` (doc-hidden) so the regression test can mount the genuine
// promoted-node body instead of a hand-copied mirror.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn render_node_body(
    ui: &mut egui::Ui,
    max_h: f32,
    idx: u32,
    meta: &proto::NodeMeta,
    ids: &[String],
    metrics: &HashMap<String, Vec<f32>>,
    edges: &[u32],
    color_by: crate::ui::state::ColorBy,
    palette: crate::data::PaletteId,
    active_filters: &crate::ui::query::ActiveFieldFilters,
    field_index: Option<&FieldIndex>,
    page_viewer_states: &mut HashMap<String, ui::page_viewer::PageViewerState>,
    markdown_cache: &mut egui_commonmark::CommonMarkCache,
    tag_query: &mut String,
    channels: &mut AnchoredChannels,
) {
    // NOTE: do NOT pre-draw a path/tags header here. The inspector body
    // below (`inspector::render_body`) already renders the node's id —
    // which for vault nodes IS the path — as its strong title row
    // (`show_metadata`), and the tags as interactive chips
    // (`show_badges`). The panel CHROME title (`node_title`) shows the
    // node name. A pre-header here re-drew the path a SECOND time (dim,
    // above the bright id row) and the tags a second time (plain text,
    // above the chips), which read as a duplicated/overlapping body that
    // squashed over the traffic-light dots (real-screenshot bug). The
    // body owns the metadata; this function just bounds + delegates.
    ui.set_max_height(max_h);
    let mut data = crate::ui::inspector::InspectorData {
        ids,
        metrics,
        edges,
        selected_idx: Some(idx),
        requested_selection: &mut channels.requested_selection,
        requested_filter_toggle: &mut channels.requested_filter_toggle,
        color_by,
        palette,
        current_meta: Some(meta),
        active_filters,
        requested_navigate: &mut channels.requested_navigate,
        requested_open_url: &mut channels.requested_open_url,
        requested_focus_node: &mut channels.requested_focus_node,
        field_index,
        page_viewer_states: Some(page_viewer_states),
        markdown_cache: Some(markdown_cache),
        requested_page_save: &mut channels.requested_page_save,
    };
    crate::ui::inspector::render_body(ui, tag_query, &mut data, Some(max_h));
}

/// Truncate a markdown body to a hover-preview snippet: up to `max_chars`
/// chars OR `max_lines` lines, whichever bound trips first. Appends "…"
/// when truncated. Preserves line breaks so multi-paragraph snippets
/// don't all run together on one line.
fn body_snippet(body: &str, max_chars: usize, max_lines: usize) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    let mut lines = 0usize;
    for line in body.lines() {
        if lines >= max_lines {
            out.push('…');
            return out;
        }
        // Skip leading blank lines so a body that starts with `\n` doesn't
        // burn a snippet "line" on emptiness.
        if out.is_empty() && line.trim().is_empty() {
            continue;
        }
        let remaining = max_chars.saturating_sub(chars);
        if remaining == 0 {
            out.push('…');
            return out;
        }
        let take = line.chars().count().min(remaining);
        out.extend(line.chars().take(take));
        if take < line.chars().count() {
            out.push('…');
            return out;
        }
        chars += take;
        lines += 1;
        if lines < max_lines && chars < max_chars {
            out.push('\n');
            chars += 1;
        }
    }
    out.trim_end().to_string()
}

fn kick_off_bootstrap(load: SharedLoad, base: String, prog: ProgressSink) {
    let client = ApiClient::new(base);

    let task = async move {
        set_status(&load, "fetching /graph/init…");
        let t_init = prog.start("bootstrap", "fetching /graph/init");
        let init = match client.init().await {
            Ok(v) => {
                prog.finish(t_init);
                v
            }
            Err(e) => {
                prog.fail(t_init, format!("{e}"));
                set_failed(&load, format!("/graph/init: {e}"));
                return;
            }
        };
        log::info!(
            "[graph-renderer] init: {} nodes, {} edges",
            init.n_nodes,
            init.n_edges
        );
        prog.info(
            "bootstrap",
            format!("init: {} nodes, {} edges", init.n_nodes, init.n_edges),
        );

        set_status(&load, "fetching /graph/ids…");
        let t_ids = prog.start("bootstrap", "fetching /graph/ids");
        let ids = match client.ids().await {
            Ok(v) => {
                prog.finish(t_ids);
                v
            }
            Err(e) => {
                prog.fail(t_ids, format!("{e}"));
                set_failed(&load, format!("/graph/ids: {e}"));
                return;
            }
        };

        set_status(&load, "fetching /graph/positions…");
        let t_pos = prog.start("bootstrap", "fetching /graph/positions");
        let positions_2d = match client.positions().await {
            Ok(v) => {
                prog.finish(t_pos);
                v
            }
            Err(e) => {
                prog.fail(t_pos, format!("{e}"));
                set_failed(&load, format!("/graph/positions: {e}"));
                return;
            }
        };

        set_status(&load, "fetching /graph/edges…");
        let t_edges = prog.start("bootstrap", "fetching /graph/edges");
        let edges = match client.edges().await {
            Ok(v) => {
                prog.finish(t_edges);
                v
            }
            Err(e) => {
                prog.fail(t_edges, format!("{e}"));
                set_failed(&load, format!("/graph/edges: {e}"));
                return;
            }
        };

        // Fetch all metrics concurrently rather than serially. Each
        // request is now an `Arc::clone` + `Cache-Control` header on the
        // server (see `graph-api/src/state.rs::binary_cache`), so the
        // round-trip is dominated by the network — and four parallel
        // hits over keep-alive cost ~one round-trip total instead of
        // four. Keeps the load-status copy reasonable by listing all
        // four names in the message.
        const METRICS: &[&str] = &["degree", "pagerank", "kcore", "community"];
        set_status(&load, format!("fetching {} metric buffers in parallel…", METRICS.len()));
        let t_metrics_all = prog.start(
            "bootstrap",
            format!("fetching {} metric buffers in parallel", METRICS.len()),
        );
        let metric_futs = METRICS.iter().map(|name| {
            let client = client.clone();
            let prog = prog.clone();
            let name = name.to_string();
            async move {
                let id = prog.start("bootstrap", format!("metric {name}"));
                let res = client.metric(&name).await;
                match &res {
                    Ok(_) => prog.finish(id),
                    Err(e) => prog.fail(id, format!("{e}")),
                }
                (name, res)
            }
        });
        let metric_results = futures::future::join_all(metric_futs).await;
        prog.finish(t_metrics_all);
        let mut metrics = std::collections::HashMap::new();
        for (name, res) in metric_results {
            match res {
                Ok(v) => {
                    metrics.insert(name, v);
                }
                Err(e) => {
                    log::warn!("[graph-renderer] metric {name}: {e}");
                    prog.warn("bootstrap", format!("metric {name}: {e}"));
                }
            }
        }

        // Ignore the server's 2D positions for spawn — seed nodes on a
        // hollow sphere shell so the force sim collapses outward from a
        // clean, isotropic distribution instead of a flat ring. Radius
        // ~800 wu lands the camera fit at a sensible zoom for graphs in
        // the few-thousand-node range.
        let positions_3d = data::spawn_on_unit_sphere(init.n_nodes as usize, 800.0);

        log::info!(
            "[graph-renderer] bootstrap fetched: {} ids, {} positions (2D), {} edges, {} metrics",
            ids.len(),
            positions_2d.len() / 2,
            edges.len() / 2,
            metrics.len()
        );

        prog.info(
            "bootstrap",
            format!(
                "fetched: {} ids, {} positions, {} edges, {} metrics",
                ids.len(),
                positions_2d.len() / 2,
                edges.len() / 2,
                metrics.len()
            ),
        );

        let bootstrap = Bootstrap {
            init: Some(init),
            ids,
            positions: positions_3d,
            edges,
            metrics,
        };
        *load.lock().unwrap() = LoadState::Ready(bootstrap);
    };

    spawn_async(task);
}

/// Server-side progress poller. Long-lived: polls `/progress?since=<seq>`
/// every 250ms (adaptive — backs off to 2s when the server has been idle
/// for a few polls in a row) and replays each backend `ProgressEvent`
/// into the renderer's local sink.
///
/// Wire shape (matches `graph_api::progress::ProgressResponse`):
/// ```json
/// { "next_seq": N, "server_ms": <unix-ms>,
///   "events": [ { "seq": S, "ts_ms": <unix-ms>,
///                 "event": { "kind": "start", "id": …, "group": …, "label": … } }, … ] }
/// ```
///
/// Server `id` / `seq` live in separate name-spaces from the
/// frontend-issued ones (the backend's `start` allocates server-side
/// task ids, the frontend's `Progress::start` allocates its own); to
/// avoid collisions we hash the server's `(group, id)` into a new
/// frontend task id maintained in a small per-poller map.
fn kick_off_progress_poll(base: String, prog: ProgressSink) {
    use std::collections::HashMap;
    use std::time::Duration;

    #[derive(serde::Deserialize)]
    struct Resp {
        next_seq: u64,
        #[allow(dead_code)]
        server_ms: u64,
        events: Vec<Stamped>,
    }
    // Server-side `seq` / `ts_ms` are parsed by serde from the JSON
    // payload but not yet used on the client: elapsed-time badges are
    // derived from `Instant::now()` at receipt, which is fine for live
    // reloads but loses sub-poll resolution for events that completed
    // before the first poll. Acceptable trade-off; reconsider if the
    // footer ever needs absolute timestamps.
    #[derive(serde::Deserialize)]
    struct Stamped {
        #[allow(dead_code)]
        seq: u64,
        #[allow(dead_code)]
        ts_ms: u64,
        event: BackendEvent,
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum BackendEvent {
        Start { id: u64, group: String, label: String },
        SetProgress { id: u64, progress: f32 },
        UpdateLabel { id: u64, label: String },
        Finish { id: u64 },
        Fail { id: u64, reason: String },
        Log { level: String, group: String, message: String },
    }

    let client = ApiClient::new(base);
    spawn_async(async move {
        // Map: backend task id -> local frontend task id we allocated
        // through `prog.start(...)`. Live for the life of the session.
        let mut id_map: HashMap<u64, u64> = HashMap::new();
        let mut cursor: u64 = 0;
        let mut idle_polls: u32 = 0;
        loop {
            match client.progress(cursor).await {
                Ok(bytes) => match serde_json::from_slice::<Resp>(&bytes) {
                    Ok(resp) => {
                        if resp.events.is_empty() {
                            idle_polls = idle_polls.saturating_add(1);
                        } else {
                            idle_polls = 0;
                            for stamped in resp.events {
                                match stamped.event {
                                    BackendEvent::Start { id, group, label } => {
                                        let local = prog.start(group, label);
                                        id_map.insert(id, local);
                                    }
                                    BackendEvent::SetProgress { id, progress } => {
                                        if let Some(&local) = id_map.get(&id) {
                                            prog.set_progress(local, progress);
                                        }
                                    }
                                    BackendEvent::UpdateLabel { id, label } => {
                                        if let Some(&local) = id_map.get(&id) {
                                            prog.update_label(local, label);
                                        }
                                    }
                                    BackendEvent::Finish { id } => {
                                        if let Some(local) = id_map.remove(&id) {
                                            prog.finish(local);
                                        }
                                    }
                                    BackendEvent::Fail { id, reason } => {
                                        if let Some(local) = id_map.remove(&id) {
                                            prog.fail(local, reason);
                                        }
                                    }
                                    BackendEvent::Log { level, group, message } => {
                                        match level.as_str() {
                                            "warn" => prog.warn(group, message),
                                            "error" => prog.error(group, message),
                                            _ => prog.info(group, message),
                                        }
                                    }
                                }
                            }
                        }
                        cursor = resp.next_seq;
                    }
                    Err(_) => {
                        // Old server (no /progress)? Back off hard so we
                        // don't spin against a 404. Future server upgrades
                        // re-bind the route and pick up on the next loop.
                        idle_polls = idle_polls.saturating_add(8);
                    }
                },
                Err(_) => {
                    idle_polls = idle_polls.saturating_add(4);
                }
            }
            // 250ms while live, back off to 2s if 8 polls in a row brought
            // nothing new.
            let delay = if idle_polls >= 8 {
                Duration::from_millis(2000)
            } else {
                Duration::from_millis(250)
            };
            sleep_async(delay).await;
        }
    });
}

/// Short rendering of a node id for status-footer labels — long path-like
/// ids ("notes/2025/projects/foo.md") would dominate the strip otherwise.
/// Keeps the last 28 chars with an ellipsis on the front.
fn short_id(id: &str) -> String {
    const MAX: usize = 28;
    if id.chars().count() <= MAX {
        return id.to_string();
    }
    let suffix: String = id.chars().rev().take(MAX).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{suffix}")
}

/// Short rendering for a search query (different policy than node ids:
/// truncate from the *end* since the head is the meaningful prefix).
fn short_label(s: &str) -> String {
    const MAX: usize = 24;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX).collect();
    format!("{head}…")
}

fn set_status(load: &SharedLoad, msg: impl Into<String>) {
    let mut guard = load.lock().unwrap();
    *guard = LoadState::Loading(msg.into());
}

fn set_failed(load: &SharedLoad, msg: String) {
    log::error!("[graph-renderer] bootstrap failed: {msg}");
    *load.lock().unwrap() = LoadState::Failed(msg);
}

/// Pure backend-routing decision for the Generate dispatch (extracted so it is
/// unit-testable without an `App`). An explicit `GenerateBackendChoice` forces
/// its backend; `Auto` picks **Server when graph-api is reachable**, else a
/// local fallback — LocalWorker on wasm, Inline on native. This mirrors the
/// local-vs-remote layout-engine default.
fn resolve_generate_backend(
    choice: crate::ui::state::GenerateBackendChoice,
    server_reachable: bool,
    is_wasm: bool,
) -> crate::job::ExecutionBackend {
    use crate::job::ExecutionBackend as Be;
    use crate::ui::state::GenerateBackendChoice as Choice;
    match choice {
        Choice::Inline => Be::Inline,
        Choice::Server => Be::Server,
        Choice::LocalWorker => Be::LocalWorker,
        Choice::Auto => {
            if server_reachable {
                Be::Server
            } else if is_wasm {
                Be::LocalWorker
            } else {
                Be::Inline
            }
        }
    }
}

/// Cross-target async sleep. `tokio::time::sleep` only works inside a
/// tokio runtime (native path); the wasm path needs `gloo_timers`.
#[cfg(target_arch = "wasm32")]
async fn sleep_async(d: std::time::Duration) {
    gloo_timers::future::TimeoutFuture::new(d.as_millis() as u32).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_async(d: std::time::Duration) {
    tokio::time::sleep(d).await;
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn spawn_async<F: std::future::Future<Output = ()> + 'static>(f: F) {
    wasm_bindgen_futures::spawn_local(f);
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_async<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime")
    });
    rt.spawn(f);
}

#[cfg(test)]
mod dispatch_tests {
    use super::resolve_generate_backend;
    use crate::job::ExecutionBackend as Be;
    use crate::ui::state::GenerateBackendChoice as Choice;

    #[test]
    fn explicit_choices_are_forced_regardless_of_reachability() {
        for &reachable in &[true, false] {
            for &is_wasm in &[true, false] {
                assert_eq!(
                    resolve_generate_backend(Choice::Inline, reachable, is_wasm),
                    Be::Inline
                );
                assert_eq!(
                    resolve_generate_backend(Choice::Server, reachable, is_wasm),
                    Be::Server
                );
                assert_eq!(
                    resolve_generate_backend(Choice::LocalWorker, reachable, is_wasm),
                    Be::LocalWorker
                );
            }
        }
    }

    #[test]
    fn auto_prefers_server_when_reachable() {
        assert_eq!(
            resolve_generate_backend(Choice::Auto, true, true),
            Be::Server
        );
        assert_eq!(
            resolve_generate_backend(Choice::Auto, true, false),
            Be::Server
        );
    }

    #[test]
    fn auto_falls_back_local_when_unreachable() {
        // wasm → LocalWorker, native → Inline.
        assert_eq!(
            resolve_generate_backend(Choice::Auto, false, true),
            Be::LocalWorker
        );
        assert_eq!(
            resolve_generate_backend(Choice::Auto, false, false),
            Be::Inline
        );
    }

    #[test]
    fn default_choice_is_auto() {
        assert_eq!(Choice::default(), Choice::Auto);
    }
}
