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
    /// Set once we emit the readiness console-log line (used by the test harness).
    logged_ready: bool,
    /// Phase E: ephemeral modal state. Not persisted — open-state is per-session.
    modal: ui::ModalState,
    /// Async slot the fetch task writes the NodeMeta into.
    node_fetch: NodeFetchSlot,
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
    /// Async-shared `/search?q=` result cache.
    search_cache: SearchCache,
    /// Search queries we've already kicked off (avoid double-fire).
    search_inflight: HashSet<String>,

    // -- "previous-frame" trackers used to gate GPU writes -----------------
    prev_style_key: Option<(SizeBy, ColorBy, ShapeBy, u32, u32, EdgeColorBy, [u32; 4], crate::data::PaletteId)>,
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
        let state: ui::AppState = ui::persist::load_from_eframe(cc.storage);

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

        // Long-lived compute-broker health watcher. Polls
        // `/compute/health` every 2s and emits a footer-log event on
        // every state transition (connected ↔ disconnected). Without
        // this signal, a stalled Remote FA2 stream reads as a frontend
        // bug — in fact graph-api is up but its gRPC dial to
        // graph-compute is failing.
        {
            let sink = progress.sink();
            let client = ApiClient::new(base_url.clone());
            spawn_async(async move {
                use std::time::Duration;
                let mut last_known: Option<bool> = None;
                loop {
                    match client.compute_health().await {
                        Ok(h) => {
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
                            // graph-api itself is down or the route 404s.
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
            logged_ready: false,
            modal: ui::ModalState::default(),
            node_fetch: Arc::new(Mutex::new(None)),
            ids: Vec::new(),
            id_to_idx: HashMap::new(),
            metrics: HashMap::new(),
            base_url,
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
            anchored_drag_offsets: HashMap::new(),
            promoted_anchored_meta: None,
            promoted_anchored_fetch: Arc::new(Mutex::new(None)),
            page_viewer_states: HashMap::new(),
            page_viewer_markdown_cache: egui_commonmark::CommonMarkCache::default(),
            save_in_flight: Arc::new(Mutex::new(None)),
            field_index: None,
            field_index_slot,
            progress,
            input_ctx: InputCtx::new(default_bindings()),

            #[cfg(target_arch = "wasm32")]
            state_pushed_hash: None,
            #[cfg(target_arch = "wasm32")]
            state_dirty: false,
            #[cfg(target_arch = "wasm32")]
            last_persist_at: None,
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
                self.field_index = Some(fi);
            }
            Err(e) => {
                log::warn!("[graph-renderer] meta_summary fetch failed: {e}");
            }
        }
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
                self.modal.open = true;
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

    /// Draw a 1px leader line from the floating-inspector window's
    /// nearest corner to the on-canvas projection of the selected node.
    ///
    /// Reads the live CPU position mirror (kept fresh by the GPU→CPU
    /// position readback so picking tracks the running force-sim) and
    /// projects through the *current* `pipes.camera` view-projection.
    /// The aspect we feed into the projection comes from `canvas_rect`
    /// rather than `pipes.camera.aspect` — same reasoning as
    /// `raycast_idx`: the GraphCallback's `prepare()` won't have run
    /// yet on this frame, so the camera's stored aspect may lag the
    /// freshly-painted canvas.
    fn draw_inspector_leader_line(
        &self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        window_rect: egui::Rect,
        canvas_rect: egui::Rect,
        idx: u32,
    ) {
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let renderer = wgpu_state.renderer.read();
        let Some(pipes) = renderer.callback_resources.get::<GraphPipelines>() else { return };
        let positions = pipes.positions_cpu();
        let i3 = (idx as usize).saturating_mul(3);
        if i3 + 2 >= positions.len() {
            return;
        }
        let world = glam::Vec3::new(positions[i3], positions[i3 + 1], positions[i3 + 2]);

        // Build a view-proj that uses the canvas rect's aspect (matches
        // what the canvas was painted with this frame, even before the
        // GraphCallback's `prepare()` runs and updates camera.aspect).
        let cam = &pipes.camera;
        let aspect = (canvas_rect.width() / canvas_rect.height().max(0.0001)).max(0.0001);
        let view = glam::Mat4::look_to_rh(cam.position, cam.forward(), glam::Vec3::Y);
        let proj = glam::Mat4::perspective_rh(cam.fov_y, aspect, cam.znear, cam.zfar);
        let clip = (proj * view) * world.extend(1.0);
        if clip.w <= 0.0 {
            return; // Behind the camera.
        }
        let ndc_x = clip.x / clip.w;
        let ndc_y = clip.y / clip.w;
        if !(-1.0..=1.0).contains(&ndc_x) || !(-1.0..=1.0).contains(&ndc_y) {
            return; // Off-screen; skip rather than clamp.
        }
        // NDC y is up; egui screen y is down — flip on the y axis.
        let screen_x = canvas_rect.left() + (ndc_x * 0.5 + 0.5) * canvas_rect.width();
        let screen_y = canvas_rect.top() + (1.0 - (ndc_y * 0.5 + 0.5)) * canvas_rect.height();
        let node_screen = egui::pos2(screen_x, screen_y);

        // Find the closest of the four window corners. Euclidean
        // distance reads cleaner as a "shortest visual line" than
        // Manhattan: the latter biases toward cardinal directions and
        // would pick awkward corners when the node sits at a 45°
        // bearing from the window centre.
        let corners = [
            window_rect.left_top(),
            window_rect.right_top(),
            window_rect.left_bottom(),
            window_rect.right_bottom(),
        ];
        let mut best = corners[0];
        let mut best_d2 = f32::INFINITY;
        for &c in &corners {
            let d2 = (c - node_screen).length_sq();
            if d2 < best_d2 {
                best_d2 = d2;
                best = c;
            }
        }

        // Foreground-layer painter sits above the canvas central panel
        // but below the floating Window (egui draws windows on top of
        // Order::Foreground via their own per-window layer). This gives
        // the visual effect of the line "coming out from under" the
        // window edge.
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("inspector-leader"),
        ));
        painter.line_segment(
            [best, node_screen],
            egui::Stroke::new(1.0, crate::ui::theme::palette::BORDER),
        );
        painter.circle_filled(node_screen, 2.5, crate::ui::theme::palette::ICON);
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
        let colors = data::colors_from_metric(
            self.state.style.color_by.metric_key(),
            &self.metrics,
            n_nodes,
            self.state.style.palette,
        );
        let sizes = data::sizes_from_metric(
            self.state.style.size_by.metric_key(),
            &self.metrics,
            n_nodes,
            apply_size_scale(self.state.style.size_mul, self.state.style.log_scale_size),
        );
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

        // Right-hand inspector panel — must run before the CentralPanel
        // so the dock area auto-shrinks to fit. The inspector reads ids /
        // metrics / edges from the cached bootstrap and can request a
        // selection change which we drain immediately afterwards.
        let mut requested_selection: Option<u32> = None;
        {
            // Snapshot a slice borrow from GraphPipelines edges — the
            // call below releases the renderer lock before egui begins.
            let edges_snapshot: Vec<u32> = if let Some(wgpu_state) = frame.wgpu_render_state() {
                let renderer = wgpu_state.renderer.read();
                renderer
                    .callback_resources
                    .get::<GraphPipelines>()
                    .map(|p| p.edges_cpu().to_vec())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let mut requested_filter_toggle: Option<(String, String)> = None;
            let mut requested_navigate: Option<String> = None;
            let mut requested_open_url: Option<String> = None;
            let mut requested_focus_node: Option<String> = None;
            {
                // Surface frontmatter-derived chips for the focused node.
                // The modal's `current` is populated by `/node/:id` fetches
                // and is keyed at the node we just selected — same source
                // the modal renders from, so the inspector and modal show
                // the same chip set for the same input.
                let current_meta = self.modal.current.as_ref();
                // Clone the active-filter snapshot so we can simultaneously
                // hand `&mut self.state` to the inspector for its open-state
                // / panel chrome — borrow checker won't let us reach into
                // `self.state.query.active_filters` while a `&mut self.state`
                // is in flight, and `ActiveFieldFilters` is cheap to clone
                // (a few small BTrees with short string keys).
                let active_filters_snapshot = self.state.query.active_filters.clone();
                let mut data = ui::inspector::InspectorData {
                    ids: &self.ids,
                    metrics: &self.metrics,
                    edges: &edges_snapshot,
                    selected_idx: self.selected_node_idx,
                    requested_selection: &mut requested_selection,
                    requested_filter_toggle: &mut requested_filter_toggle,
                    color_by: self.state.style.color_by,
                    palette: self.state.style.palette,
                    current_meta,
                    active_filters: &active_filters_snapshot,
                    requested_navigate: &mut requested_navigate,
                    requested_open_url: &mut requested_open_url,
                    requested_focus_node: &mut requested_focus_node,
                    field_index: self.field_index.as_ref(),
                };
                ui::inspector::show_floating(ctx, &mut self.state, &mut data);
                let inspector_rect: Option<egui::Rect> = None;
                // Floating-mode leader line: when the inspector is a
                // floating window AND a node is selected AND the canvas
                // is mounted, draw a 1px line from the window's nearest
                // corner to the selected node's on-canvas position.
                // Skipped when the node is off-screen (clip-w ≤ 0 or NDC
                // outside [-1,1]) — simpler than clamping to the canvas
                // edge and reads cleanly: line just disappears as the
                // user pans the node out of view, reappears when it
                // returns.
                if let (Some(win_rect), Some(canvas_rect), Some(idx)) = (
                    inspector_rect,
                    self.prev_canvas_rect,
                    self.selected_node_idx,
                ) {
                    self.draw_inspector_leader_line(ctx, frame, win_rect, canvas_rect, idx);
                }
            }
            if let Some((f, v)) = requested_filter_toggle {
                self.state.query.toggle_field_filter(&f, &v);
            }
            if let Some(id) = requested_focus_node {
                // focus_node_by_id handles the modal refresh internally,
                // so don't double-dispatch via `requested_navigate` below.
                self.focus_node_by_id(frame, &id);
            } else if let Some(target) = requested_navigate {
                self.kick_off_node_fetch(target);
            }
            if let Some(href) = requested_open_url {
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
        }

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
                self.execute_action(frame, &action_id, params);
            }
            PaletteOutcome::OpenNode { id } => {
                self.kick_off_node_fetch(id);
            }
            PaletteOutcome::None => {}
        }

        // Status footer — sits at the bottom of the screen, above the
        // central panel which auto-shrinks to fit. Registered after the
        // sidebar/inspector so the side panels still own the full
        // height of the screen.
        ui::status_footer::show_tray(ctx, &mut self.state, &self.progress);

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
            crate::ui::floating::FloatingPanel::new(
                ui::state::PanelId::Canvas,
                "Graph",
            )
            .default_pos([120.0, 80.0])
            .default_size([900.0, 600.0])
            .show(ctx, &mut canvas_open, |ui| {
                let mut viewer = ui::workspace::WorkspaceViewer { ctx: &mut wctx };
                viewer.draw_graph_tab(ui);
            });
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
                    let mut style = egui_dock::Style::from_egui(ui.style());
                    if n_tabs <= 1 {
                        style.tab_bar.height = 0.0;
                        style.tab_bar.bg_fill = egui::Color32::TRANSPARENT;
                    }
                    egui_dock::DockArea::new(&mut self.state.dock.dock_state)
                        .show_add_buttons(n_tabs > 1)
                        .show_add_popup(n_tabs > 1)
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
            self.modal.open = false;
            self.modal.current = None;
            self.modal.fetch_error = None;
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
            if let Some(idx) = self.raycast_idx(frame, ndc_x, ndc_y, [rect_w, rect_h]) {
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
                    // UX: surface the inspector if the user clicks a node
                    // while it's collapsed. They almost certainly want to
                    // see what they just clicked.
                    if !self.state.inspector_open {
                        self.state.inspector_open = true;
                    }
                    self.kick_off_node_fetch(id);
                }
            } else {
                // Click on empty canvas → clear sticky focus.
                self.focus_sticky_idx = None;
            }
        }

        // Hover-driven focus (throttled). Sticky click takes precedence.
        self.update_hover_focus(frame, pointer_in_canvas, canvas_rect);
        // Hover-preview card delay/fetch state machine. Reads
        // `focus_hover_idx` set above. The actual paint happens in
        // `show_hover_preview` at the end of `update` so it sits on
        // top of the existing UI layers.
        self.tick_hover_preview(pointer_in_canvas);

        // Inspector requested a different selection (clicked a community
        // sibling or neighbor pill). Drive the full focus path: camera
        // slides to the node, sticky highlight follows, modal refreshes.
        // `focus_node_by_id` internally calls kick_off_node_fetch, so the
        // sidebar updates the same way it did before.
        if let Some(idx) = requested_selection.take() {
            self.selected_node_idx = Some(idx);
            if !self.state.inspector_open {
                self.state.inspector_open = true;
            }
            if let Some(id) = self.id_for_idx(idx) {
                log::info!(
                    "[graph-renderer] inspector selected idx={} id={}",
                    idx,
                    id
                );
                self.focus_node_by_id(frame, &id);
            }
        }

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

        self.perf.begin_stage(StageId::ApplyEffects);
        self.apply_focus_to_gpu(frame);
        self.apply_camera_to_gpu(ctx, frame);
        self.apply_cursor_force(frame);
        self.tick_post_click_cooldown(frame);
        self.perf.end_stage(StageId::ApplyEffects);

        // Hover-preview card paint pass — runs after all other UI
        // layers so the card sits on top of canvas + sidebars. Cheap:
        // no-op when `hover_preview_open == false`.
        self.show_hover_preview(ctx, frame);

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
            || self.cursor_force_active.abs() > 0.0;
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

    fn style_key(&self) -> (SizeBy, ColorBy, ShapeBy, u32, u32, EdgeColorBy, [u32; 4], crate::data::PaletteId) {
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
        let colors = data::colors_from_metric(
            self.state.style.color_by.metric_key(),
            &self.metrics,
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
            &self.metrics,
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
                        &self.metrics,
                        n,
                        pipes.edges_cpu(),
                        edge_fallback,
                        self.state.style.palette,
                    )
                };
                pipes.update_edge_colors(&queue, edge_colors);
            }
        }
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
        let hit = self.raycast_idx(frame, ndc_x, ndc_y, [rect_w, rect_h]);
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
    fn show_hover_preview(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        // Drain the promoted-anchored fetch slot first so we have
        // fresh meta available for this frame's paint.
        if let Some(Ok(Some(meta))) =
            self.promoted_anchored_fetch.lock().unwrap().take()
        {
            self.promoted_anchored_meta = Some(meta);
        }

        // Drain any completed page-save result and route the outcome
        // back to the matching PageViewerState. On success, also
        // refresh the cached `promoted_anchored_meta.body` so the
        // dirty-detector resets.
        if let Some((node_id, body, result)) =
            self.save_in_flight.lock().unwrap().take()
        {
            if let Some(state) = self.page_viewer_states.get_mut(&node_id) {
                match &result {
                    Ok(()) => {
                        state.note_saved(body.clone());
                    }
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

        let Some(canvas_rect) = self.prev_canvas_rect else {
            return;
        };

        // Two anchored panels can be live in one frame: a hovered
        // preview (transient, non-promoted) and a sticky promoted
        // panel. Render both, but skip the hover preview when its
        // idx matches the promoted idx — they'd render at the same
        // anchor and the user would see a doubled card.
        let hover_idx = if self.hover_preview_open {
            self.hover_preview_idx
        } else {
            None
        };
        let promoted_idx = self.promoted_anchored_idx;

        // Render promoted first (it's "below" in semantic stack —
        // the hover preview is the more transient layer). Both use
        // egui::Area::order(Foreground); paint order within the
        // layer falls in call order, but they have different ids and
        // never overlap (we skip hover when idx matches).
        if let Some(pidx) = promoted_idx {
            if let Some(meta) = self.promoted_anchored_meta.clone() {
                self.render_anchored_panel(ctx, frame, canvas_rect, pidx, meta, true);
            }
        }

        if let Some(hidx) = hover_idx {
            if Some(hidx) != promoted_idx {
                if let Some(meta) = self.hover_preview_meta.clone() {
                    self.render_anchored_panel(ctx, frame, canvas_rect, hidx, meta, false);
                }
            }
        }
    }

    /// Render a single anchored panel for `idx`.
    ///
    /// `promoted = true` enables the X close button (clears
    /// `promoted_anchored_idx`) and makes the body scrollable. Both
    /// variants share the EMA-smoothed positioning + soft-tether drag
    /// pipeline: project once, blend into `last_anchored_screen_pos`,
    /// hand the smoothed value to AnchoredPanel as
    /// `screen_pos_override`, then accumulate any per-frame drag
    /// delta into `anchored_drag_offsets[idx]`.
    ///
    /// The header is built inside the body closure (egui doesn't give
    /// AnchoredPanel a separate header surface), which means the
    /// "header drag" is detected via the OUTER area's drag_delta in
    /// `AnchoredOutput` — egui's Area drag handling moves the area
    /// when *anywhere on it* is dragged, but we only use the delta to
    /// update our per-node offset; the Area itself is `fixed_pos`,
    /// so this won't conflict.
    fn render_anchored_panel(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        canvas_rect: egui::Rect,
        idx: u32,
        meta: proto::NodeMeta,
        promoted: bool,
    ) {
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

        // Soft-tether: per-node drag offset accumulated across frames.
        // Mutates only inside the closure on click; passed in as a
        // value here so AnchoredPanel sees the current snapshot.
        let drag_offset = self.anchored_drag_offsets.get(&idx).copied();

        // resnap_requested is set by the header's "↺" button or a
        // double-click. We can't write back to self inside the
        // closure (mutable borrow of self), so we capture into a
        // Cell. Pattern mirrors how modal close buttons forward
        // intents up through the show() call.
        let resnap_flag = std::cell::Cell::new(false);
        let close_flag = std::cell::Cell::new(false);

        let panel_id_tag = if promoted { "anchored-promoted" } else { "hover-preview" };
        let panel = crate::ui::anchored::AnchoredPanel::new(
            egui::Id::new((panel_id_tag, idx)),
            world,
            canvas_rect,
            &camera,
        )
        .offset(egui::vec2(18.0, 18.0))
        .interactable(true)
        .anchor_pixels(drag_offset)
        .screen_pos_override(smoothed);

        let output = panel.show(ctx, |ui| {
            ui.set_max_width(360.0);

            // Header is a dedicated drag-sensing strip. Allocate it
            // first as a click_and_drag rect of fixed height; lay the
            // glyphs/buttons over it via a `UiBuilder` at the same
            // rect. The returned `header_resp` is what AnchoredPanel
            // reads `drag_delta()` / `double_clicked()` from — body
            // widgets (e.g. the markdown ScrollArea) sense their own
            // gestures without their drag bubbling up to move the
            // panel.
            let header_height = 22.0;
            let header_resp = ui.allocate_response(
                egui::vec2(ui.available_width(), header_height),
                egui::Sense::click_and_drag(),
            );
            let header_rect = header_resp.rect;
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(header_rect), |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("\u{2630}")
                            .small()
                            .color(crate::ui::theme::palette::ICON),
                    );
                    let title = if meta.title.is_empty() {
                        meta.id.clone()
                    } else {
                        meta.title.clone()
                    };
                    ui.label(
                        egui::RichText::new(title)
                            .strong()
                            .color(crate::ui::theme::palette::TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if promoted {
                            if ui
                                .small_button(egui::RichText::new("\u{2715}").color(
                                    crate::ui::theme::palette::ICON,
                                ))
                                .on_hover_text("Close")
                                .clicked()
                            {
                                close_flag.set(true);
                            }
                        }
                        if ui
                            .small_button(egui::RichText::new("\u{21BA}").color(
                                crate::ui::theme::palette::ICON,
                            ))
                            .on_hover_text("Re-snap to anchor")
                            .clicked()
                        {
                            resnap_flag.set(true);
                        }
                    });
                });
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
            // Obsidian-page promoted panel: route the body slot to the
            // editable page-viewer (Rendered / Source tabs + Save). For
            // hover-preview (non-promoted) and non-page nodes, fall back
            // to the original inline markdown/snippet view.
            let use_page_viewer =
                promoted && crate::ui::page_viewer::is_obsidian_page(&meta);
            if use_page_viewer {
                ui.separator();
                let state = self
                    .page_viewer_states
                    .entry(meta.id.clone())
                    .or_default();
                let mut save_request: Option<(String, String)> = None;
                {
                    let mut on_save = |path: &str, body: &str| {
                        save_request = Some((path.to_string(), body.to_string()));
                    };
                    let mut actions = crate::ui::page_viewer::PageViewerActions {
                        markdown_cache: &mut self.page_viewer_markdown_cache,
                        on_save: &mut on_save,
                    };
                    crate::ui::page_viewer::show_in_panel(
                        ui,
                        state,
                        &meta,
                        &mut actions,
                    );
                }
                if let Some((path, body)) = save_request {
                    self.kick_off_page_save(meta.id.clone(), path, body);
                }
            } else if !meta.body.is_empty() {
                ui.separator();
                if promoted {
                    // Promoted variant: full body, scrollable. We
                    // keep `drag_to_scroll` enabled (default) so the
                    // user can click+drag inside the body to scroll;
                    // the header is the only drag-sensing surface
                    // upstream, so this drag stays inside ScrollArea
                    // and never reaches AnchoredPanel.
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&meta.body)
                                    .small()
                                    .color(crate::ui::theme::palette::TEXT),
                            );
                        });
                } else {
                    let snippet = body_snippet(&meta.body, 280, 6);
                    ui.label(
                        egui::RichText::new(snippet)
                            .small()
                            .color(crate::ui::theme::palette::TEXT),
                    );
                }
            }

            // Hand the header response back to AnchoredPanel — that's
            // what its `drag_delta` / `header_double_clicked` come
            // from.
            ((), header_resp)
        });

        // Accumulate drag delta into per-node offset — only for the
        // promoted variant. The hover preview is a transient peek
        // that vanishes on cursor-leave; a drag there would be lost
        // immediately, and we'd rather not pollute the per-node
        // offset map with hover-induced jitter.
        if promoted && output.drag_delta != egui::Vec2::ZERO {
            let entry = self
                .anchored_drag_offsets
                .entry(idx)
                .or_insert(egui::Vec2::ZERO);
            *entry += output.drag_delta;
        }
        // Header double-click is the canonical re-snap gesture.
        if output.header_double_clicked {
            resnap_flag.set(true);
        }
        if resnap_flag.get() {
            self.anchored_drag_offsets.remove(&idx);
        }
        if close_flag.get() && promoted {
            self.promoted_anchored_idx = None;
            self.promoted_anchored_meta = None;
            // Don't clear the drag offset — if the user later
            // re-promotes this node, restoring their offset is the
            // friendlier default.
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
            ToggleInspector => {
                self.state.inspector_open = !self.state.inspector_open;
                serde_json::json!({ "inspector_open": self.state.inspector_open })
            }
        }
    }
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
fn spawn_async<F: std::future::Future<Output = ()> + 'static>(f: F) {
    wasm_bindgen_futures::spawn_local(f);
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_async<F: std::future::Future<Output = ()> + Send + 'static>(f: F) {
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
