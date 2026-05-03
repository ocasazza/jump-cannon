use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::data::{self, Bootstrap, LoadState, SharedLoad};
use crate::fetch::ApiClient;
use crate::graph_callback::GraphCallback;
use crate::graph_pipelines::{GraphData, GraphPipelines};
use crate::proto;
use crate::ui;
use crate::ui::actions::{self, ActionRegistry, BuiltinAction, ParamValue};
use crate::ui::command_palette::PaletteOutcome;
use crate::ui::query::EvalContext;
use crate::ui::state::{ColorBy, FontFamilyChoice, SizeBy};
use graph_layouts::GpuForceOptions;

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

    // -- auto-fit dedup ----------------------------------------------------
    /// Last canvas size we ran `fit_camera()` for. Auto-refit only fires
    /// when this changes (window resize). Following live graph bounds
    /// caused click-blackouts: the cursor force perturbs the sim, bounds
    /// spike, refit zooms way out, sub-pixel cull blanks the screen.
    /// Manual refit via `F`, the Camera section button, or Ctrl+P → Fit
    /// Camera covers the rest.
    last_fit_screen: Option<egui::Vec2>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Phase D theme: B&W high-contrast with RGBY accents.
        ui::apply_theme(&cc.egui_ctx);

        // Restore persisted UI state (active section, slider values, etc).
        let mut state: ui::AppState = cc
            .storage
            .and_then(|s| s.get_string(ui::STORAGE_KEY))
            .and_then(|s| serde_json::from_str::<ui::AppState>(&s).ok())
            .unwrap_or_default();

        // Migrate stale persisted layout settings from earlier defaults.
        // Cap steps_per_call so old persisted 8.0 doesn't burn the GPU;
        // leave the cooling/energy knobs alone so the user's tuned
        // values survive and the new slower-cool defaults are reachable.
        state.layout.steps_per_call = state.layout.steps_per_call.min(4.0);

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
            last_fit_screen: None,
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
        let Some(bootstrap) = bootstrap_opt else {
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
        ui::show_sidebar(ctx, &mut self.state, &mut self.actions);

        // Command palette modal — runs above the sidebar, below the modal.
        let palette_outcome = ui::show_command_palette(
            ctx,
            &mut self.palette_state,
            &mut self.actions,
            &self.state.workspace,
        );
        if let PaletteOutcome::Execute { action_id, params } = palette_outcome {
            self.execute_action(frame, &action_id, params);
        }

        // Phase B central panel — wgpu graph layer via egui_wgpu callback.
        // Frame is transparent so the wgpu output isn't covered.
        let mut click: Option<(egui::Rect, egui::Pos2)> = None;
        let mut canvas_rect: Option<egui::Rect> = None;
        let mut pointer_in_canvas: Option<egui::Pos2> = None;
        let mut lmb_held = false;
        let mut rmb_held = false;
        // Snapshot the load message so we can render a progress overlay
        // before the graph buffers exist. Cheap clone; the lock is brief.
        let load_msg: Option<String> = {
            let guard = self.load.lock().unwrap();
            match &*guard {
                LoadState::Pending => Some("loading…".to_string()),
                LoadState::Loading(m) => Some(m.clone()),
                _ => None,
            }
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::TRANSPARENT))
            .show(ctx, |ui| {
                let avail = ui.available_size();
                let (rect, resp) =
                    ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
                // Skip the wgpu graph callback entirely while the bootstrap
                // is still loading — there's nothing to draw yet, and
                // showing a progress label gives the user a signal that
                // something is happening instead of a blank canvas.
                if self.loaded_into_gpu {
                    let cb = GraphCallback {
                        screen_px: [rect.width().max(1.0), rect.height().max(1.0)],
                    };
                    ui.painter()
                        .add(egui_wgpu::Callback::new_paint_callback(rect, cb));
                } else if let Some(msg) = load_msg.as_ref() {
                    // Centered white label at ~14px on the dark cosmograph
                    // background (clear_color is set above the callback).
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        msg,
                        egui::FontId::proportional(14.0),
                        egui::Color32::WHITE,
                    );
                }
                canvas_rect = Some(rect);

                if let Some(pos) = resp.hover_pos().or_else(|| resp.interact_pointer_pos()) {
                    if rect.contains(pos) {
                        pointer_in_canvas = Some(pos);
                    }
                }

                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        click = Some((rect, pos));
                    }
                }

                // LMB / RMB held → cursor force tool. egui's response gives
                // us drag state for the primary button; secondary is read
                // from the pointer state.
                lmb_held = resp.dragged_by(egui::PointerButton::Primary)
                    || ui.input(|i| {
                        i.pointer
                            .button_down(egui::PointerButton::Primary)
                            && resp.hovered()
                    });
                rmb_held = ui.input(|i| {
                    i.pointer.button_down(egui::PointerButton::Secondary) && resp.hovered()
                });

                // Apply camera-invert toggles to drag deltas. We only
                // touch the camera if the user is dragging inside the
                // canvas, so the sidebar sliders aren't fighting it.
                if resp.dragged_by(egui::PointerButton::Primary) && !lmb_held {
                    // (no-op branch — see below; cursor force takes over LMB drag)
                }
                if resp.dragged_by(egui::PointerButton::Middle) {
                    let d = resp.drag_delta();
                    let mut dx = d.x;
                    let mut dy = d.y;
                    if self.state.camera.invert_mouse_x {
                        dx = -dx;
                    }
                    if self.state.camera.invert_mouse_y {
                        dy = -dy;
                    }
                    if let Some(wgpu_state) = frame.wgpu_render_state() {
                        let mut renderer = wgpu_state.renderer.write();
                        if let Some(pipes) =
                            renderer.callback_resources.get_mut::<GraphPipelines>()
                        {
                            pipes.camera.rotate_yaw(dx * 0.005);
                            pipes.camera.rotate_pitch(-dy * 0.005);
                        }
                    }
                }
            });

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
                    self.kick_off_node_fetch(id);
                }
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
        self.apply_style_to_gpu(frame);
        self.apply_layout_to_gpu(frame);
        self.apply_focus_to_gpu(frame);
        self.apply_camera_to_gpu(frame);
        self.apply_cursor_force(frame);
        self.tick_post_click_cooldown(frame);
        self.apply_selection(frame);
        self.refresh_stats(frame);

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
            // While the sim is warm (KE well above the halt threshold)
            // the user can't perceive 60fps of layout shuffle. Throttle
            // to ~20fps so we waste less GPU on imperceptible frames.
            // Pointer input bypasses the throttle for snappy interaction.
            let warm_threshold = self.state.layout.energy_threshold * 5.0;
            let warm = self.loaded_into_gpu
                && !sim_settled
                && self.last_observed_max_ke > warm_threshold
                && self.cursor_force_active.abs() == 0.0
                && self.post_click_cooldown_frames == 0
                && !has_user_input;
            if warm {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            } else {
                ctx.request_repaint();
            }
        } else {
            // Light tick so a fresh user action (e.g. an action instance
            // mutating state) isn't held up for an arbitrary time.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
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

    fn layout_key(&self) -> u64 {
        let l = &self.state.layout;
        // Bit-pack the slider floats into a hash.
        let bits = [
            l.repulsion.to_bits(),
            l.spring_k.to_bits(),
            l.spring_len.to_bits(),
            l.gravity.to_bits(),
            l.damping.to_bits(),
            l.dt.to_bits(),
            l.steps_per_call.to_bits(),
            l.cooling_alpha.to_bits(),
            l.cooling_floor.to_bits(),
            l.energy_threshold.to_bits(),
        ];
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bits {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    fn apply_layout_to_gpu(&mut self, frame: &mut eframe::Frame) {
        if !self.loaded_into_gpu {
            return;
        }
        let key = self.layout_key();
        if self.prev_layout_key == Some(key) {
            return;
        }
        let Some(wgpu_state) = frame.wgpu_render_state() else { return };
        let mut renderer = wgpu_state.renderer.write();
        if let Some(pipes) = renderer.callback_resources.get_mut::<GraphPipelines>() {
            // Start from existing options so we keep cursor pos / grid_enabled / cooling.
            let mut opts = pipes.layout_options().unwrap_or_else(GpuForceOptions::default);
            let l = &self.state.layout;
            opts.repulsion = l.repulsion;
            opts.spring_k = l.spring_k;
            opts.spring_len = l.spring_len;
            opts.gravity = l.gravity;
            opts.damping = l.damping;
            opts.dt = l.dt;
            opts.steps_per_call = l.steps_per_call.max(1.0) as u32;
            // Cooling: damping decays toward floor each call so kinetic
            // energy bleeds off and the layout reaches steady state.
            opts.cooling_alpha = l.cooling_alpha;
            opts.cooling_floor = l.cooling_floor;
            opts.energy_threshold = l.energy_threshold;
            // Repulsion radius scales with spring_len so the spatial-hash
            // grid bounds per-pair work to a 27-cell neighborhood.
            opts.repulsion_radius = (4.0 * l.spring_len).max(1.0);
            pipes.update_layout_options(opts);
        }
        self.prev_layout_key = Some(key);
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

    fn apply_camera_to_gpu(&mut self, frame: &mut eframe::Frame) {
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
            // Auto-refit ONLY on canvas resize. Following the live graph
            // bounds is too aggressive: a click fires the cursor force
            // for one frame which perturbs the sim, bounds spike, the
            // refit zooms out, all nodes become sub-pixel and cull to
            // a blank canvas for the duration of the disturbance.
            // Initial fit happens once at load (pipes.fit_to_loaded_bounds
            // in load()). Manual refit via Ctrl+P → Fit Camera or `F`.
            let screen = self
                .prev_canvas_rect
                .map(|r| r.size())
                .unwrap_or(egui::vec2(0.0, 0.0));
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
