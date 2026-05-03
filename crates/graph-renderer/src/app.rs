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
use crate::ui::layout::registry::LayoutRegistry;
use crate::ui::query::EvalContext;
use crate::ui::state::{ColorBy, FontFamilyChoice, SizeBy};
use graph_layouts::{warmup_positions, GpuForceOptions};

/// Result of an async `/node/:id` fetch — Some(Ok) success, Some(Err) error,
/// None means no fetch has completed since the last poll.
type NodeFetchSlot = Arc<Mutex<Option<Result<proto::NodeMeta, String>>>>;

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
    prev_style_key: Option<(SizeBy, ColorBy, u32)>,
    prev_layout_key: Option<u64>,
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Phase D theme: B&W high-contrast with RGBY accents.
        ui::apply_theme(&cc.egui_ctx);

        // Restore persisted UI state (active section, slider values, etc).
        // Run the layout-shape migration on the raw JSON value first so
        // pre-refactor `LayoutState { repulsion, spring_k, ... }` blobs
        // get folded into the new `{ active, settings: { "gpu-force":
        // {...} } }` shape before serde decodes the typed struct.
        let state: ui::AppState = cc
            .storage
            .and_then(|s| s.get_string(ui::STORAGE_KEY))
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .map(|mut v| {
                ui::state::migrate_layout_state(&mut v);
                v
            })
            .and_then(|v| serde_json::from_value::<ui::AppState>(v).ok())
            .unwrap_or_default();

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
        kick_off_bootstrap(load.clone(), base_url.clone());

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
            prev_focus_key: None,
            prev_cursor_key: None,
            prev_selected_hash: None,
            prev_canvas_rect: None,
            last_pointer_in_canvas: None,
            cursor_force_active: 0.0,
            prev_cursor_force_active: 0.0,
            post_click_cooldown_frames: 0,
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
                spawn_async(async move {
                    let result = client.node(&id_for_task).await;
                    *slot.lock().unwrap() = Some((id_for_task, result));
                });
            }
        }
    }

    /// Spawn an async `/node/:id` fetch. The result lands in `self.node_fetch`
    /// and gets drained into the modal on the next frame's `update`.
    fn kick_off_node_fetch(&self, id: String) {
        let slot = self.node_fetch.clone();
        let client = ApiClient::new(self.base_url.clone());
        spawn_async(async move {
            let result = client.node(&id).await;
            *slot.lock().unwrap() = Some(result);
        });
    }

    /// Drain a completed `/node/:id` fetch into the modal state, if any.
    fn drain_node_fetch(&mut self) {
        let result_opt = self.node_fetch.lock().unwrap().take();
        let Some(result) = result_opt else { return };
        match result {
            Ok(meta) => {
                log::info!("[graph-renderer] modal: fetched node {}", meta.id);
                self.modal.fetch_error = None;
                self.modal.current = Some(meta);
                self.modal.open = true;
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
    fn raycast_idx(&self, frame: &eframe::Frame, ndc_x: f32, ndc_y: f32) -> Option<u32> {
        let wgpu_state = frame.wgpu_render_state()?;
        let renderer = wgpu_state.renderer.read();
        let pipes = renderer.callback_resources.get::<GraphPipelines>()?;
        pipes.raycast(ndc_x, ndc_y)
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
        let warmed = warmup_positions(n_nodes, &bootstrap.edges, spring_len, 0xC0A75E);
        if warmed.len() == bootstrap.positions.len() {
            bootstrap.positions = warmed;
            log::info!(
                "[graph-renderer] coarsening warmup applied ({} nodes)",
                n_nodes
            );
        }

        // Initial colors / sizes from the user's persisted style choice.
        let colors = data::colors_from_metric(
            self.state.style.color_by.metric_key(),
            &self.metrics,
            n_nodes,
        );
        let sizes = data::sizes_from_metric(
            self.state.style.size_by.metric_key(),
            &self.metrics,
            n_nodes,
            self.state.style.size_mul,
        );
        let graph = GraphData {
            positions: bootstrap.positions,
            edges: bootstrap.edges,
            colors,
            sizes,
        };

        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            match pipes.load(&device, &queue, graph) {
                Ok(()) => {
                    log::info!(
                        "[graph-renderer] graph loaded: {} nodes, {} edges",
                        pipes.n_nodes(),
                        pipes.n_edges()
                    );
                    self.loaded_into_gpu = true;
                }
                Err(e) => {
                    log::error!("[graph-renderer] GraphPipelines::load failed: {e}");
                }
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
        if let Ok(json) = serde_json::to_string(&self.state) {
            storage.set_string(ui::STORAGE_KEY, json);
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.perf.begin_frame();
        // Re-apply theme each frame so hot edits to theme.rs land without restart.
        ui::apply_theme(ctx);

        // Pump the data pipeline.
        self.try_promote_bootstrap_to_gpu(frame);
        self.emit_ready_log(frame);

        // Drain any completed /node/:id fetch into the modal.
        self.drain_node_fetch();

        // Ctrl+P (or ⌘P on macOS) toggles the command palette.
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::P)) {
            self.palette_state.toggle();
        }
        // Bare F (no modifiers, no text-edit focus) fits the camera —
        // mirrors the shortcut hint shown on the Fit Camera action.
        let f_pressed = ctx.input(|i| {
            i.key_pressed(egui::Key::F)
                && !i.modifiers.command
                && !i.modifiers.shift
                && !i.modifiers.alt
        });
        if f_pressed && !ctx.wants_keyboard_input() {
            self.execute_action(frame, "fit-camera", HashMap::new());
        }

        // Phase D sidebar (activity bar + section panel) on the left.
        self.perf.begin_stage(StageId::UiChrome);
        ui::show_sidebar(
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
            let mut data = ui::inspector::InspectorData {
                ids: &self.ids,
                metrics: &self.metrics,
                edges: &edges_snapshot,
                selected_idx: self.selected_node_idx,
                requested_selection: &mut requested_selection,
            };
            ui::inspector::show(ctx, &mut self.state, &mut data);
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
            canvas_rect: None,
            pointer_in_canvas: None,
            click: None,
            lmb_held: false,
            rmb_held: false,
            add_tab_requests: Vec::new(),
            split_requests: Vec::new(),
        };
        self.perf.begin_stage(StageId::EguiPaint);
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::TRANSPARENT))
            .show(ctx, |ui| {
                let mut viewer = ui::workspace::WorkspaceViewer { ctx: &mut wctx };
                egui_dock::DockArea::new(&mut self.state.dock.dock_state)
                    .show_add_buttons(true)
                    .show_add_popup(true)
                    .style(egui_dock::Style::from_egui(ui.style()))
                    .show_inside(ui, &mut viewer);
            });
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
        let lmb_held = wctx.lmb_held;
        let rmb_held = wctx.rmb_held;

        self.prev_canvas_rect = canvas_rect;
        self.last_pointer_in_canvas = pointer_in_canvas;
        // Cursor force sign: LMB attract (negative), RMB repel (positive),
        // matching the cheatsheet labels.
        self.cursor_force_active = if lmb_held {
            -1.0
        } else if rmb_held {
            1.0
        } else {
            0.0
        };

        // Esc closes the modal.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.modal.open = false;
            self.modal.current = None;
            self.modal.fetch_error = None;
        }

        if let Some((rect, pos)) = click {
            let ndc_x = (pos.x - rect.left()) / rect.width().max(1.0) * 2.0 - 1.0;
            let ndc_y = -((pos.y - rect.top()) / rect.height().max(1.0) * 2.0 - 1.0);
            if let Some(idx) = self.raycast_idx(frame, ndc_x, ndc_y) {
                if let Some(id) = self.id_for_idx(idx) {
                    log::info!(
                        "[graph-renderer] click hit node idx={} id={}",
                        idx,
                        id
                    );
                    self.selected_node_idx = Some(idx);
                    // UX: surface the inspector if the user clicks a node
                    // while it's collapsed. They almost certainly want to
                    // see what they just clicked.
                    if !self.state.inspector_open {
                        self.state.inspector_open = true;
                    }
                    self.kick_off_node_fetch(id);
                }
            }
        }

        // Inspector requested a different selection (clicked a community
        // sibling or neighbor row). Drive the same path the canvas click
        // uses: update selected idx + kick the /node/:id fetch.
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
                self.kick_off_node_fetch(id);
            }
        }

        // Draw the modal — last so it stacks above the central panel.
        let action = ui::show_modal(ctx, &mut self.modal);
        if let Some(target) = action.navigate_to {
            self.kick_off_node_fetch(target);
        }
        if let Some((field, value)) = action.toggle_filter {
            self.append_filter_card(field, value);
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

        self.perf.begin_stage(StageId::ApplySelection);
        self.apply_selection(frame);
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
        self.state.active_section = Some(ui::Section::Filter);
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
            spawn_async(async move {
                match client.search(&q_owned).await {
                    Ok(results) => {
                        let mut set: HashSet<u32> = HashSet::new();
                        for id in results.ids {
                            if let Some(&idx) = id_to_idx.get(&id) {
                                set.insert(idx);
                            }
                        }
                        cache.lock().unwrap().insert(q_owned, set);
                    }
                    Err(e) => {
                        log::warn!("[graph-renderer] /search failed: {e}");
                        // Insert empty so we don't loop forever on bad query.
                        cache.lock().unwrap().insert(q_owned, HashSet::new());
                    }
                }
            });
        }
    }

    fn style_key(&self) -> (SizeBy, ColorBy, u32) {
        (
            self.state.style.size_by,
            self.state.style.color_by,
            self.state.style.size_mul.to_bits(),
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
                );
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
        );
        let sizes = data::sizes_from_metric(
            self.state.style.size_by.metric_key(),
            &self.metrics,
            n,
            self.state.style.size_mul,
        );
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            pipes.update_colors(&queue, colors);
            pipes.update_sizes(&queue, sizes);
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
                self.state
                    .layout
                    .settings
                    .insert(active_id.clone(), factory.default_settings());
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

        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            if let Some(factory) = self.layout_registry.get(&active_id) {
                match factory.kind() {
                    graph_layouts::LayoutKind::Physics => {
                        if active_changed {
                            pipes.swap_physics_layout(&device, &queue, factory, &json_owned);
                        } else {
                            pipes.set_physics_layout_settings_json(&json_owned);
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
    fn tick_post_click_cooldown(&mut self, frame: &mut eframe::Frame) {
        if self.post_click_cooldown_frames == 0 || !self.loaded_into_gpu {
            return;
        }
        self.post_click_cooldown_frames -= 1;
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            if let Some(mut opts) = pipes.layout_options() {
                // Aggressive cooling tweaks — only for the cooldown window.
                opts.cooling_alpha *= 0.95;
                opts.energy_threshold *= 5.0;
                pipes.update_layout_options(opts);
            }
        }
        if self.post_click_cooldown_frames == 0 {
            // Restore the user's tuned values via apply_layout_to_gpu's
            // normal path on the next frame.
            self.prev_layout_key = None;
        }
    }

    fn apply_selection(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let cache = self.search_cache.lock().unwrap().clone();
        let ctx = EvalContext::new(&self.ids, &self.id_to_idx, &cache);
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
                self.state.active_section = Some(sec);
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

fn kick_off_bootstrap(load: SharedLoad, base: String) {
    let client = ApiClient::new(base);

    let task = async move {
        set_status(&load, "fetching /graph/init…");
        let init = match client.init().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/init: {e}"));
                return;
            }
        };
        log::info!(
            "[graph-renderer] init: {} nodes, {} edges",
            init.n_nodes,
            init.n_edges
        );

        set_status(&load, "fetching /graph/ids…");
        let ids = match client.ids().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/ids: {e}"));
                return;
            }
        };

        set_status(&load, "fetching /graph/positions…");
        let positions_2d = match client.positions().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/positions: {e}"));
                return;
            }
        };

        set_status(&load, "fetching /graph/edges…");
        let edges = match client.edges().await {
            Ok(v) => v,
            Err(e) => {
                set_failed(&load, format!("/graph/edges: {e}"));
                return;
            }
        };

        let mut metrics = std::collections::HashMap::new();
        for name in ["degree", "pagerank", "kcore", "community"] {
            set_status(&load, format!("fetching /graph/metrics/{name}…"));
            match client.metric(name).await {
                Ok(v) => {
                    metrics.insert(name.to_string(), v);
                }
                Err(e) => {
                    log::warn!("[graph-renderer] metric {name}: {e}");
                }
            }
        }

        let positions_3d = data::promote_2d_to_3d(&positions_2d, init.n_nodes as u64);

        log::info!(
            "[graph-renderer] bootstrap fetched: {} ids, {} positions (2D), {} edges, {} metrics",
            ids.len(),
            positions_2d.len() / 2,
            edges.len() / 2,
            metrics.len()
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

fn set_status(load: &SharedLoad, msg: impl Into<String>) {
    let mut guard = load.lock().unwrap();
    *guard = LoadState::Loading(msg.into());
}

fn set_failed(load: &SharedLoad, msg: String) {
    log::error!("[graph-renderer] bootstrap failed: {msg}");
    *load.lock().unwrap() = LoadState::Failed(msg);
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
