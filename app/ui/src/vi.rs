//! Vi-style graph traversal, DOM half: key translation, the graph cursor and
//! its selection sync, camera follow, and the `/` hand-off to the Nodes panel.
//!
//! Keyboard semantics live in `panel_kit_core::{keymap, graph_nav}`; this
//! module only turns Dioxus events into `KeyChord`s and applies the resolved
//! intents to `Ctx` and the renderer. Pure parts are in `nav.rs`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::events::KeyboardEvent;
use dioxus::prelude::*;
use glam::Vec3;
use panel_kit_core::graph_nav::{GraphCursor, GraphView, NavEffect, Side};
use panel_kit_core::keymap::tour::{Tour, TourEvent};
use panel_kit_core::keymap::{chord_display, Candidate, IntentVocabulary, KeymapState, Resolved};
use panel_kit::input::key_chord;
use panel_kit_core::Key as CoreKey;
use wasm_bindgen::JsCast;

use crate::api;
use crate::graph_canvas::GraphData;
use crate::nav::{AppVerb, GraphIndex, Intent, KEYMAP, TOUR};
use crate::{render, Ctx, Panel, CLOSE_PANEL, OPEN_PANEL};

/// DOM id of the Nodes panel search box, focused by `nav.search`.
pub(crate) const SEARCH_INPUT_ID: &str = "nodes-search";

struct ViState {
    keys: KeymapState,
    cursor: GraphCursor<u32>,
    /// Graph session the cursor belongs to.
    epoch: u64,
    /// Camera re-aims at the cursor after every move.
    follow: bool,
    /// A `nav.search` prompt is open: Nodes results feed the cursor matches.
    searching: bool,
    /// The first result of this search already moved the cursor.
    search_moved: bool,
    /// Active tour, if `:` started one.
    tour: Option<Tour<Intent>>,
}

/// The app context, captured by the driver so palette actions can run intents.
static CTX: GlobalSignal<Option<Ctx>> = Signal::global(|| None);

impl ViState {
    fn new() -> Self {
        Self {
            keys: KeymapState::default(),
            cursor: GraphCursor::default(),
            epoch: 0,
            follow: true,
            searching: false,
            search_moved: false,
            tour: None,
        }
    }
}

static VI: GlobalSignal<ViState> = Signal::global(ViState::new);

/// What the which-key strip shows; the only vi state components subscribe to.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Hint {
    /// Chords of the unfinished sequence, space separated.
    pub typed: String,
    /// Count typed so far.
    pub count: Option<u32>,
    /// Rules still reachable from the pending sequence.
    pub candidates: Vec<Candidate>,
    /// Mode name and index into the keymap's modes.
    pub mode: String,
    pub mode_ix: usize,
    /// Echo line: what the last key did, a tip, or the tour prompt.
    pub message: Option<Echo>,
    /// Vi cursor degree `(incoming, outgoing)` for the degree badge.
    pub degree: Option<(usize, usize)>,
}

/// One echo-line message and how long it stays.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Echo {
    pub text: String,
    pub kind: EchoKind,
    /// Serial so a later message's expiry never clears a newer one.
    pub serial: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EchoKind {
    /// What the last intent did; 1.5 s.
    Did,
    /// Why the last intent did nothing; 2.5 s.
    Refused,
    /// A one-time tip; 4 s.
    Tip,
    /// The active tour prompt; stays until the step completes.
    Tour,
}

impl EchoKind {
    fn ttl_ms(self) -> Option<u32> {
        match self {
            Self::Did => Some(1500),
            Self::Refused => Some(2500),
            Self::Tip => Some(4000),
            Self::Tour => None,
        }
    }
}

static ECHO_SERIAL: GlobalSignal<u64> = Signal::global(|| 0);

/// Show one echo-line message; timed kinds clear themselves unless replaced.
pub(crate) fn echo(text: impl Into<String>, kind: EchoKind) {
    let serial = *ECHO_SERIAL.peek() + 1;
    *ECHO_SERIAL.write() = serial;
    let text = text.into();
    {
        let mut hint = HINT.write();
        hint.message = Some(Echo { text, kind, serial });
    }
    if let Some(ttl) = kind.ttl_ms() {
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(ttl).await;
            let mut hint = HINT.write();
            if hint.message.as_ref().is_some_and(|m| m.serial == serial) {
                hint.message = None;
            }
        });
    }
}

/// Show the active tour's prompt as a persistent echo.
fn echo_tour(vi: &ViState) {
    if let Some(tour) = &vi.tour {
        let (at, total) = tour.progress();
        let text = if tour.finished() {
            tour.prompt().to_owned()
        } else {
            format!("Tour {}/{total}: {}", at + 1, tour.prompt())
        };
        let kind = if tour.finished() { EchoKind::Tip } else { EchoKind::Tour };
        echo(text, kind);
    }
}

// --- one-time tips --------------------------------------------------------------

const TIPS_KEY: &str = "jc_vi_tips_v1";

/// Show `text` once per browser profile for tip `id`.
pub(crate) fn tip_once(id: &str, text: &str) {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    let mut seen: Vec<String> = storage
        .get_item(TIPS_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if seen.iter().any(|s| s == id) {
        return;
    }
    seen.push(id.to_owned());
    if let Ok(raw) = serde_json::to_string(&seen) {
        let _ = storage.set_item(TIPS_KEY, &raw);
    }
    echo(text, EchoKind::Tip);
}

pub(crate) static HINT: GlobalSignal<Hint> = Signal::global(Hint::default);

fn set_hint(hint: Hint) {
    if *HINT.peek() != hint {
        *HINT.write() = hint;
    }
}

fn hint_of(vi: &ViState, candidates: Vec<Candidate>) -> Hint {
    let prev = HINT.peek();
    Hint {
        typed: vi.keys.pending.iter().map(|c| chord_display(*c)).collect::<Vec<_>>().join(" "),
        count: vi.keys.count,
        candidates,
        mode: KEYMAP.mode_name(&vi.keys).to_owned(),
        mode_ix: vi.keys.mode,
        message: prev.message.clone(),
        degree: prev.degree,
    }
}

/// Keyboard posture for the mode line.
pub(crate) fn posture_label(focused: Option<Panel>) -> String {
    use panel_kit_core::keymap::Posture;
    use panel_kit_core::FocusContext;
    let focus = if panel_kit::input::is_editing() {
        FocusContext::TextInput
    } else {
        match focused {
            None | Some(Panel::Graph) => FocusContext::Workspace,
            Some(p) => FocusContext::Panel(p),
        }
    };
    let vi = VI.peek();
    match KEYMAP.posture(&vi.keys, &focus) {
        Posture::Keymap { mode, count, pending } => {
            let mut s = mode.to_uppercase();
            if let Some(c) = count {
                s.push_str(&format!(" · {c}"));
            }
            if !pending.is_empty() {
                s.push_str(&format!(" · {pending}…"));
            }
            s
        }
        Posture::Panel(p) => {
            use panel_kit::PanelKind;
            format!("PANEL {} · Tab/Esc to leave", p.title())
        }
        Posture::TextInput => "INSERT".to_owned(),
    }
}

/// Recompute the degree badge for the cursor node.
fn refresh_degree(vi: &ViState, index: &GraphIndex) {
    let degree = vi.cursor.current.map(|idx| {
        (
            index.neighbors(&idx, None, Side::Domain).len(),
            index.neighbors(&idx, None, Side::Codomain).len(),
        )
    });
    let mut hint = HINT.write();
    if hint.degree != degree {
        hint.degree = degree;
    }
}

// --- graph index cache ---------------------------------------------------------

/// Identity of the graph a cached index was built from. The session epoch
/// changes on every graph replacement, so equal keys mean the same topology.
#[derive(Clone, Copy, PartialEq, Eq)]
struct IndexKey {
    epoch: u64,
    n_nodes: u32,
    n_edges: u32,
}

impl IndexKey {
    fn of(g: &GraphData, epoch: u64) -> Self {
        Self {
            epoch,
            n_nodes: g.n_nodes,
            n_edges: g.n_edges,
        }
    }
}

thread_local! {
    static INDEX: RefCell<Option<(IndexKey, Rc<GraphIndex>)>> = const { RefCell::new(None) };
    /// Key of the edge-kind fetch in flight or already answered.
    static KINDS_ASKED: RefCell<Option<IndexKey>> = const { RefCell::new(None) };
}

/// The index of the mounted graph, built on first use per graph session.
/// Untyped until the edge kinds arrive (see [`ViDriver`]).
pub(crate) fn index_for(g: &GraphData, epoch: u64) -> Rc<GraphIndex> {
    let key = IndexKey::of(g, epoch);
    INDEX.with(|cell| {
        let mut cell = cell.borrow_mut();
        if let Some((k, index)) = cell.as_ref() {
            if *k == key {
                return index.clone();
            }
        }
        let index = Rc::new(GraphIndex::build(&g.ids, &g.scene.edges, None));
        *cell = Some((key, index.clone()));
        index
    })
}

/// Fetch the edge kinds of the mounted server graph once and re-index with
/// them. A missing endpoint, an error or a snapshot-revision mismatch leaves
/// the untyped index in place.
fn request_edge_kinds(ctx: Ctx, key: IndexKey) {
    if KINDS_ASKED.with(|k| k.borrow().is_some_and(|asked| asked == key)) {
        return;
    }
    KINDS_ASKED.with(|k| *k.borrow_mut() = Some(key));
    spawn(async move {
        let fetched = api::revisioned_edge_kinds().await;
        let session = ctx.graph_session.peek().clone();
        if session.epoch != key.epoch {
            return;
        }
        let kinds = match fetched {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!("[vi] edge kinds: {e}");
                return;
            }
        };
        let graph = ctx.graph.peek();
        let Some(g) = graph.as_ref() else { return };
        if IndexKey::of(g, session.epoch) != key {
            return;
        }
        let expected = session.graph_revision;
        if kinds.revision != 0 && expected.is_some() && Some(kinds.revision) != expected {
            tracing::warn!(
                "[vi] edge kinds: revision {} != mounted graph {:?}",
                kinds.revision,
                expected
            );
            return;
        }
        let index = Rc::new(GraphIndex::build(&g.ids, &g.scene.edges, Some(&kinds.value)));
        INDEX.with(|cell| *cell.borrow_mut() = Some((key, index)));
    });
}

// --- actions on the host -------------------------------------------------------

thread_local! {
    /// Node the camera was last centred on; the standoff for the next hop is
    /// measured from it, so hopping never changes the zoom level.
    static CENTERED: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Re-aim the camera at node `idx`, keeping the standoff it has from the
/// node it was last centred on (measuring from `idx` itself would add the
/// hop length every move).
fn center_on(idx: u32) {
    render::with_host(|h| {
        let positions = h.pipes.positions_cpu();
        let point_of = |i: u32| {
            let at = i as usize * 3;
            positions.get(at..at + 3).map(|p| Vec3::new(p[0], p[1], p[2]))
        };
        let Some(point) = point_of(idx) else {
            return;
        };
        let anchor = CENTERED.with(|c| c.get()).and_then(point_of).unwrap_or(point);
        let distance = (anchor - h.pipes.camera.position).length();
        h.pipes.camera.look_at_point(point, distance);
        CENTERED.with(|c| c.set(Some(idx)));
    });
}

fn select_node(ctx: Ctx, idx: u32) {
    let id = ctx
        .graph
        .peek()
        .as_ref()
        .and_then(|g| g.ids.get(idx as usize).cloned());
    let mut selected = ctx.selected;
    if let Some(id) = id {
        if selected.peek().as_deref() != Some(id.as_str()) {
            selected.set(Some(id));
        }
    }
}

fn element_by_id(id: &str) -> Option<web_sys::HtmlElement> {
    web_sys::window()?
        .document()?
        .get_element_by_id(id)?
        .dyn_into()
        .ok()
}

/// Open the Nodes panel and focus its search box once it is mounted.
fn focus_search() {
    *OPEN_PANEL.write() = Some(Panel::Nodes);
    spawn(async {
        gloo_timers::future::TimeoutFuture::new(60).await;
        if let Some(input) = element_by_id(SEARCH_INPUT_ID) {
            let _ = input.focus();
        }
    });
}

/// Whether a `/` search prompt is waiting for the Nodes panel.
pub(crate) fn search_active() -> bool {
    VI.peek().searching
}

/// Close the search prompt (Enter/Escape in the search box) and hand the
/// keyboard back to the graph; matches stay for `n`/`N`.
pub(crate) fn finish_search() {
    VI.write().searching = false;
    if let Some(input) = element_by_id(SEARCH_INPUT_ID) {
        let _ = input.blur();
    }
}

enum After {
    None,
    Select(u32),
    Search,
    Center,
    Fit,
    Open(Panel),
    ToggleFollow,
    /// `?`: show the Help panel, or dismiss it to the dock if it is visible.
    ToggleHelp,
}

/// Feed one keydown to the vi keymap. Returns whether the event was consumed
/// (default prevented); `false` means the chord is unbound and the camera and
/// workspace handlers should see it.
pub(crate) fn handle_key(event: &KeyboardEvent, ctx: Ctx) -> bool {
    let Some(chord) = key_chord(event) else {
        return false;
    };
    let (index, n_nodes) = {
        let graph = ctx.graph.peek();
        let Some(g) = graph.as_ref() else {
            return false;
        };
        (index_for(g, ctx.graph_session.peek().epoch), g.ids.len())
    };
    if n_nodes == 0 {
        return false;
    }

    let bare = !(chord.shift || chord.alt || chord.ctrl || chord.meta);

    let mut vi = VI.write();
    let tour_active = vi.tour.as_ref().is_some_and(|t| !t.finished());
    let nothing_pending = vi.keys.pending.is_empty() && vi.keys.count.is_none();
    if chord.key == CoreKey::Escape && bare && nothing_pending && tour_active {
        vi.tour = None;
        drop(vi);
        echo("tour ended", EchoKind::Did);
        event.prevent_default();
        return true;
    }
    let resolved = KEYMAP.feed(&mut vi.keys, chord);
    let (intent, count) = match resolved {
        Resolved::Unbound(_) => {
            let hint = hint_of(&vi, Vec::new());
            drop(vi);
            set_hint(hint);
            return false;
        }
        Resolved::Cancelled => {
            let hint = hint_of(&vi, Vec::new());
            drop(vi);
            set_hint(hint);
            event.prevent_default();
            return true;
        }
        Resolved::Pending { candidates } => {
            let hint = hint_of(&vi, candidates);
            drop(vi);
            set_hint(hint);
            event.prevent_default();
            return true;
        }
        Resolved::Intent { intent, count } => (intent, count),
    };
    drop(vi);
    event.prevent_default();
    apply_intent(ctx, index, intent, count);
    true
}

/// Run an intent from the palette, exactly as a keypress would.
pub(crate) fn run_intent(intent: Intent, count: Option<u32>) {
    let Some(ctx) = *CTX.peek() else { return };
    let index = {
        let graph = ctx.graph.peek();
        let Some(g) = graph.as_ref() else { return };
        index_for(g, ctx.graph_session.peek().epoch)
    };
    apply_intent(ctx, index, intent, count);
}

fn apply_intent(ctx: Ctx, index: Rc<GraphIndex>, intent: Intent, count: Option<u32>) {
    let mut vi = VI.write();
    vi.searching = false;
    let label = {
        let l = KEYMAP
            .rules_in(vi.keys.mode)
            .find(|r| Intent::parse_intent(r.intent).ok().as_ref() == Some(&intent))
            .map(|r| r.label.to_owned());
        l.unwrap_or_else(|| format!("{intent:?}"))
    };
    let did = match count {
        Some(c) if c > 1 => format!("{c} × {label}"),
        _ => label.clone(),
    };
    let mut message: Option<(String, EchoKind)> = Some((did, EchoKind::Did));
    let after = match &intent {
        Intent::Nav(verb) => {
            let seeded = vi.cursor.current.is_none();
            if seeded {
                vi.cursor.select(0);
            }
            match vi.cursor.apply(&*index, index.sorts(), verb, count) {
                NavEffect::Moved => vi.cursor.current.map_or(After::None, After::Select),
                NavEffect::PromptSearch => {
                    vi.searching = true;
                    vi.search_moved = false;
                    message = Some(("type, then Enter · n/N step matches".into(), EchoKind::Did));
                    After::Search
                }
                NavEffect::Unchanged => After::None,
                NavEffect::NoTarget(_) if seeded => {
                    vi.cursor.current.map_or(After::None, After::Select)
                }
                NavEffect::NoTarget(reason) => {
                    message = Some((reason.to_string(), EchoKind::Refused));
                    After::None
                }
            }
        }
        Intent::App(AppVerb::Center) => After::Center,
        Intent::App(AppVerb::Fit) => After::Fit,
        Intent::App(AppVerb::Open) => After::Open(Panel::Document),
        Intent::App(AppVerb::Inspect) => After::Open(Panel::Inspector),
        Intent::App(AppVerb::ToggleFollow) => {
            vi.follow = !vi.follow;
            message = Some((
                if vi.follow { "camera follows cursor" } else { "camera stays put" }.into(),
                EchoKind::Did,
            ));
            After::ToggleFollow
        }
        Intent::App(AppVerb::WhichKey) => {
            message = None;
            After::ToggleHelp
        }
        Intent::App(AppVerb::Tour) => {
            vi.tour = Some(TOUR.clone());
            message = None;
            After::None
        }
    };
    // Tour: the fired intent may complete the current step.
    let mut tour_echo = false;
    if let Some(tour) = vi.tour.as_mut() {
        if !matches!(intent, Intent::App(AppVerb::Tour)) {
            match tour.observe(&intent) {
                TourEvent::Advanced { after, .. } => {
                    if let Some(after) = after {
                        message = Some((after, EchoKind::Did));
                    }
                    tour_echo = true;
                }
                TourEvent::Ignored => tour_echo = message.as_ref().is_some_and(|m| m.1 == EchoKind::Did),
                TourEvent::Finished => {}
            }
        } else {
            tour_echo = true;
        }
    }
    let follow = vi.follow;
    let current = vi.cursor.current;
    refresh_degree(&vi, &index);
    let hint = hint_of(&vi, Vec::new());
    let tour_state = vi.tour.clone();
    drop(vi);
    set_hint(hint);
    let had_message = message.is_some();
    if let Some((text, kind)) = message {
        echo(text, kind);
    }
    if tour_echo {
        if let Some(tour) = &tour_state {
            if had_message && !tour.finished() {
                // Show what happened briefly, then the next prompt.
                spawn(async move {
                    gloo_timers::future::TimeoutFuture::new(1800).await;
                    let vi = VI.peek();
                    echo_tour(&vi);
                });
            } else {
                let vi = VI.peek();
                echo_tour(&vi);
            }
        }
    }

    match after {
        After::None => {}
        After::Select(idx) => {
            select_node(ctx, idx);
            if follow {
                center_on(idx);
            }
        }
        After::Search => focus_search(),
        After::Center => {
            if let Some(idx) = current {
                center_on(idx);
            }
        }
        After::Fit => render::fit_camera(),
        After::Open(panel) => {
            if let Some(idx) = current {
                select_node(ctx, idx);
            }
            *OPEN_PANEL.write() = Some(panel);
        }
        After::ToggleHelp => {
            if crate::panel_visible(Panel::Help) {
                *CLOSE_PANEL.write() = Some(Panel::Help);
            } else {
                *OPEN_PANEL.write() = Some(Panel::Help);
            }
        }
        After::ToggleFollow => {
            if let (true, Some(idx)) = (follow, current) {
                center_on(idx);
            }
        }
    }
}

// --- drivers -------------------------------------------------------------------

/// Root-mounted, render-empty driver: keeps the cursor and index in step with
/// the mounted graph, the selection and the Nodes panel search.
pub(crate) fn driver(ctx: Ctx) -> Element {
    rsx! {
        ViDriver { ctx }
    }
}

#[component]
fn ViDriver(ctx: Ctx) -> Element {
    use_hook(move || *CTX.write() = Some(ctx));

    // Graph replaced: drop the cursor, build the index, ask for edge kinds.
    use_effect(move || {
        let session = ctx.graph_session.read().clone();
        let graph = ctx.graph.read();
        if graph.is_some() {
            tip_once("first-load", "? keys · ] start · : tour");
        }
        if VI.peek().epoch != session.epoch {
            let mut vi = VI.write();
            let follow = vi.follow;
            *vi = ViState::new();
            vi.follow = follow;
            vi.epoch = session.epoch;
            CENTERED.with(|c| c.set(None));
            let hint = hint_of(&vi, Vec::new());
            drop(vi);
            set_hint(hint);
        }
        let Some(g) = graph.as_ref() else { return };
        let index = index_for(g, session.epoch);
        if session.is_server_backed() && !index.is_typed() {
            request_edge_kinds(ctx, IndexKey::of(g, session.epoch));
        }
    });

    // Selection changed anywhere (canvas click, inspector pill, palette):
    // move the cursor there so the jumplist records it.
    use_effect(move || {
        let selected = ctx.selected.read().clone();
        let graph = ctx.graph.read();
        let (Some(id), Some(g)) = (selected, graph.as_ref()) else {
            return;
        };
        let Some(&idx) = g.id_to_idx.get(&id) else {
            return;
        };
        if VI.peek().cursor.current != Some(idx) {
            VI.write().cursor.select(idx);
        }
        let index = index_for(g, ctx.graph_session.peek().epoch);
        refresh_degree(&VI.peek(), &index);
        tip_once("first-select", "h/l walk edges from here · j/k siblings");
    });

    // `/` search: server-backed graphs use the Nodes panel's ranked results;
    // client-only graphs (no /search) match ids with the local index.
    use_effect(move || {
        let session = ctx.graph_session.read().clone();
        let server = session.is_server_backed();
        let results = if server { ctx.results.read().clone() } else { Vec::new() };
        let query = if server { String::new() } else { ctx.query.read().clone() };
        let graph = ctx.graph.read();
        let Some(g) = graph.as_ref() else { return };
        if !VI.peek().searching {
            return;
        }
        let (query, matches): (String, Vec<u32>) = if server {
            let hits = results.iter().filter_map(|id| g.id_to_idx.get(id).copied()).collect();
            (ctx.query.peek().clone(), hits)
        } else {
            let index = index_for(g, session.epoch);
            let hits = index.search(&query);
            (query, hits)
        };
        let first = matches.first().copied();
        if first.is_some() {
            tip_once("first-search", "n/N step matches · Enter returns to the graph");
        }
        let follow = {
            let mut vi = VI.write();
            vi.cursor.query = query;
            vi.cursor.matches = matches;
            if let Some(first) = first {
                if vi.search_moved {
                    vi.cursor.current = Some(first);
                } else {
                    vi.cursor.select(first);
                    vi.search_moved = true;
                }
            }
            vi.follow
        };
        if let Some(first) = first {
            select_node(ctx, first);
            if follow {
                center_on(first);
            }
        }
    });

    rsx! {}
}
