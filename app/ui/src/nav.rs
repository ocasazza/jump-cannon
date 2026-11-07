//! Vi-style graph traversal, pure half: the client graph index, the intent
//! vocabulary and the default keymap. DOM, signals and camera live in `vi.rs`.
//!
//! [`GraphIndex`] is built once per graph revision from the flat `[src, tgt]`
//! edge buffer plus the optional per-edge kind slots, and implements
//! `panel_kit_core::graph_nav::GraphView` with node indices as ids.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use panel_kit_core::graph_nav::{GraphView, NavVerb, Side, SortMap};
use panel_kit_core::keymap::tour::{Tour, TourSpec};
use panel_kit_core::keymap::{IntentVocabulary, Keymap, KeymapSpec};

use crate::api::EdgeKinds;

/// Kind name of an edge with kind slot 0 (no declared type).
pub(crate) const UNTYPED: &str = "untyped";

/// Default keymap layer, authored as data.
const KEYMAP_VI_JSON: &str = include_str!("../assets/keymap-vi.json");
const TOUR_VI_JSON: &str = include_str!("../assets/tour-vi.json");

/// Compressed adjacency rows: `to[off[n]..off[n + 1]]` are node `n`'s
/// neighbours in edge order, `slot` the parallel kind slots.
struct Csr {
    off: Vec<u32>,
    to: Vec<u32>,
    slot: Vec<u16>,
}

impl Csr {
    /// Rows keyed by the first element of each `(from, to, slot)`. Edge order
    /// is kept within a row; repeated `(to, slot)` pairs collapse to the first.
    fn build(n: usize, edges: impl Iterator<Item = (u32, u32, u16)> + Clone) -> Self {
        let mut off = vec![0u32; n + 1];
        for (from, _, _) in edges.clone() {
            off[from as usize + 1] += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        let mut next = off.clone();
        let m = off[n] as usize;
        let mut to = vec![0u32; m];
        let mut slot = vec![0u16; m];
        for (from, target, s) in edges {
            let at = &mut next[from as usize];
            to[*at as usize] = target;
            slot[*at as usize] = s;
            *at += 1;
        }

        let mut seen: HashSet<(u32, u16)> = HashSet::new();
        let mut w = 0usize;
        let mut packed = vec![0u32; n + 1];
        for row in 0..n {
            let (a, b) = (off[row] as usize, off[row + 1] as usize);
            if b - a <= 1 {
                for i in a..b {
                    to[w] = to[i];
                    slot[w] = slot[i];
                    w += 1;
                }
            } else {
                seen.clear();
                for i in a..b {
                    if seen.insert((to[i], slot[i])) {
                        to[w] = to[i];
                        slot[w] = slot[i];
                        w += 1;
                    }
                }
            }
            packed[row + 1] = w as u32;
        }
        to.truncate(w);
        slot.truncate(w);
        Self {
            off: packed,
            to,
            slot,
        }
    }

    fn row(&self, n: u32) -> (&[u32], &[u16]) {
        let n = n as usize;
        match (self.off.get(n), self.off.get(n + 1)) {
            (Some(&a), Some(&b)) => (&self.to[a as usize..b as usize], &self.slot[a as usize..b as usize]),
            _ => (&[], &[]),
        }
    }
}

/// Forward and reverse adjacency of one graph revision, with edge kinds.
pub(crate) struct GraphIndex {
    n: u32,
    /// Lower-cased node ids for substring search, in node order.
    lower_ids: Vec<Box<str>>,
    /// Kind name per slot: `kind_names[0]` is [`UNTYPED`], slot `k` is the
    /// server palette's `kinds[k - 1]`.
    kind_names: Vec<String>,
    forward: Csr,
    reverse: Csr,
    sorts: SortMap,
}

impl GraphIndex {
    /// Index `ids` and the flat `[src, tgt]` `edges`. `kinds` supplies the
    /// per-edge slots; a missing or length-mismatched buffer leaves every edge
    /// untyped. Self-loops and out-of-range endpoints are dropped.
    pub(crate) fn build(ids: &[String], edges: &[u32], kinds: Option<&EdgeKinds>) -> Self {
        let n = ids.len();
        let mut kind_names = Vec::with_capacity(1 + kinds.map_or(0, |k| k.kinds.len()));
        kind_names.push(UNTYPED.to_owned());
        let slots: Option<&[u16]> = kinds.and_then(|k| {
            (k.slots.len() * 2 == edges.len() - edges.len() % 2).then_some(k.slots.as_slice())
        });
        if let (Some(k), Some(_)) = (kinds, slots) {
            kind_names.extend(k.kinds.iter().cloned());
        }
        let slot_of = |e: usize| -> u16 {
            slots
                .and_then(|s| s.get(e).copied())
                .filter(|&s| (s as usize) < kind_names.len())
                .unwrap_or(0)
        };
        let live = edges
            .chunks_exact(2)
            .enumerate()
            .filter(|(_, p)| p[0] != p[1] && (p[0] as usize) < n && (p[1] as usize) < n)
            .map(|(e, p)| (p[0], p[1], slot_of(e)));
        let forward = Csr::build(n, live.clone());
        let reverse = Csr::build(n, live.map(|(s, t, k)| (t, s, k)));
        let sorts = SortMap::from_sorting(kind_names.iter().map(|k| (k.as_str(), k.as_str())));
        Self {
            n: n as u32,
            lower_ids: ids.iter().map(|id| id.to_lowercase().into()).collect(),
            kind_names,
            forward,
            reverse,
            sorts,
        }
    }

    /// Whether per-edge kinds were applied (otherwise every edge is untyped).
    pub(crate) fn is_typed(&self) -> bool {
        self.kind_names.len() > 1
    }

    /// Identity sort map: each edge kind, including [`UNTYPED`], is its own
    /// sort.
    pub(crate) fn sorts(&self) -> &SortMap {
        &self.sorts
    }

    /// Distinct neighbours of `idx` with link direction, ascending by index:
    /// `(neighbour, has edge idx -> neighbour, has edge neighbour -> idx)`.
    pub(crate) fn adjacent(&self, idx: u32) -> Vec<(u32, bool, bool)> {
        let mut map: BTreeMap<u32, (bool, bool)> = BTreeMap::new();
        for &t in self.forward.row(idx).0 {
            map.entry(t).or_default().0 = true;
        }
        for &s in self.reverse.row(idx).0 {
            map.entry(s).or_default().1 = true;
        }
        map.into_iter().map(|(i, (out, inn))| (i, out, inn)).collect()
    }

    fn slots_of(&self, kinds: &[String]) -> Vec<u16> {
        self.kind_names
            .iter()
            .enumerate()
            .filter(|(_, name)| kinds.contains(name))
            .map(|(slot, _)| slot as u16)
            .collect()
    }
}

impl GraphView for GraphIndex {
    type Id = u32;

    fn ids(&self) -> Vec<u32> {
        (0..self.n).collect()
    }

    /// Neighbours in edge order, one entry per distinct node. `Domain` reads
    /// edges into `id` (their sources), `Codomain` edges out of it.
    fn neighbors(&self, id: &u32, kinds: Option<&[String]>, side: Side) -> Vec<u32> {
        let csr = match side {
            Side::Domain => &self.reverse,
            Side::Codomain => &self.forward,
        };
        let (to, slot) = csr.row(*id);
        let allowed = kinds.map(|k| self.slots_of(k));
        let mut seen: HashSet<u32> = HashSet::with_capacity(to.len());
        to.iter()
            .zip(slot)
            .filter(|(_, s)| allowed.as_ref().is_none_or(|a| a.contains(s)))
            .filter(|(t, _)| seen.insert(**t))
            .map(|(t, _)| *t)
            .collect()
    }

    /// Case-insensitive substring over node ids, in node order; an empty
    /// query matches nothing. The server search behind the Nodes panel is
    /// asynchronous and ranked; `vi.rs` uses it instead when the graph is
    /// server-backed.
    fn search(&self, query: &str) -> Vec<u32> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.lower_ids
            .iter()
            .enumerate()
            .filter(|(_, id)| id.contains(&q))
            .map(|(i, _)| i as u32)
            .collect()
    }
}

/// Host-side verbs the keymap can also bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AppVerb {
    /// Aim the camera at the cursor node.
    Center,
    /// Fit the camera to the graph bounds.
    Fit,
    /// Select the cursor node and open the Document panel.
    Open,
    /// Select the cursor node and open the Inspector panel.
    Inspect,
    /// Toggle the camera following the cursor.
    ToggleFollow,
    /// Toggle the full key listing for the current mode.
    WhichKey,
    /// Start (or restart) the guided tour.
    Tour,
}

impl IntentVocabulary for AppVerb {
    fn parse_intent(intent: &str) -> Result<Self, String> {
        match intent.trim() {
            "app.center" => Ok(Self::Center),
            "app.fit" => Ok(Self::Fit),
            "app.open" => Ok(Self::Open),
            "app.inspect" => Ok(Self::Inspect),
            "app.toggle_follow" => Ok(Self::ToggleFollow),
            "app.which_key" => Ok(Self::WhichKey),
            "app.tour" => Ok(Self::Tour),
            other => Err(format!("unknown app intent `{other}`")),
        }
    }
}

/// The vocabulary of the vi keymap: graph traversal or a host verb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    Nav(NavVerb),
    App(AppVerb),
}

impl IntentVocabulary for Intent {
    fn parse_intent(intent: &str) -> Result<Self, String> {
        let nav_err = match NavVerb::parse_intent(intent) {
            Ok(verb) => return Ok(Self::Nav(verb)),
            Err(e) => e,
        };
        match AppVerb::parse_intent(intent) {
            Ok(verb) => Ok(Self::App(verb)),
            // A `nav.*` failure keeps its specific reason.
            Err(_) if intent.trim_start().starts_with("nav.") => Err(nav_err),
            Err(app_err) => Err(app_err),
        }
    }
}

/// The compiled default keymap.
pub(crate) static KEYMAP: LazyLock<Keymap<Intent>> = LazyLock::new(|| {
    let spec: KeymapSpec =
        serde_json::from_str(KEYMAP_VI_JSON).expect("assets/keymap-vi.json is valid KeymapSpec JSON");
    Keymap::compile(&[spec]).expect("assets/keymap-vi.json compiles against the Intent vocabulary")
});

/// The compiled tour; cloned per run so each has its own position.
pub(crate) static TOUR: LazyLock<Tour<Intent>> = LazyLock::new(|| {
    let spec: TourSpec =
        serde_json::from_str(TOUR_VI_JSON).expect("assets/tour-vi.json is valid TourSpec JSON");
    Tour::compile(&spec).expect("assets/tour-vi.json compiles against the Intent vocabulary")
});

/// Rules the palette can run directly: every rule without a capture.
pub(crate) fn palette_rules() -> Vec<(String, String, Option<String>, Intent)> {
    KEYMAP
        .rules_in(0)
        .filter(|r| !r.keys.contains("{a-z}"))
        .filter_map(|r| {
            Intent::parse_intent(r.intent)
                .ok()
                .map(|i| (r.keys.to_owned(), r.label.to_owned(), r.group.map(str::to_owned), i))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use panel_kit_core::keymap::tour::TourEvent;

    #[test]
    fn keymap_rules_all_carry_labels_and_groups() {
        let spec: KeymapSpec = serde_json::from_str(KEYMAP_VI_JSON).unwrap();
        for r in &spec.rules {
            assert!(r.label.as_deref().is_some_and(|l| !l.is_empty()), "{} has no label", r.keys);
            assert!(r.group.is_some(), "{} has no group", r.keys);
        }
        assert!(KEYMAP.rules_in(0).all(|r| r.label != r.intent));
    }

    #[test]
    fn tour_compiles_against_intent_and_is_walkable() {
        let mut tour = TOUR.clone();
        assert_eq!(tour.progress(), (0, 9));
        let spec: TourSpec = serde_json::from_str(TOUR_VI_JSON).unwrap();
        for step in &spec.steps {
            let intent = Intent::parse_intent(&step.expect).unwrap();
            assert!(matches!(tour.observe(&intent), TourEvent::Advanced { .. }));
        }
        assert!(tour.finished());
    }

    #[test]
    fn palette_rules_skip_captures_and_cover_the_rest() {
        let rules = palette_rules();
        let spec: KeymapSpec = serde_json::from_str(KEYMAP_VI_JSON).unwrap();
        let expected = spec.rules.iter().filter(|r| !r.keys.contains("{a-z}")).count();
        assert_eq!(rules.len(), expected);
        assert!(rules.iter().all(|(k, ..)| !k.contains('{')));
    }
    use panel_kit_core::graph_nav::{GraphCursor, NavEffect};
    use panel_kit_core::keymap::{KeymapState, Resolved};
    use panel_kit_core::{Key, KeyChord};

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    fn kinds(palette: &[&str], slots: &[u16]) -> EdgeKinds {
        EdgeKinds {
            kinds: palette.iter().map(|s| (*s).to_owned()).collect(),
            slots: slots.to_vec(),
        }
    }

    fn names(ks: &[&str]) -> Vec<String> {
        ks.iter().map(|s| (*s).to_owned()).collect()
    }

    /// 0=a 1=b 2=c 3=d; edges in order:
    /// a->b calls, a->c untyped, b->c calls, c->a owns, a->b calls (dup), d->d calls (loop).
    fn sample() -> GraphIndex {
        GraphIndex::build(
            &ids(&["a", "b", "c", "d"]),
            &[0, 1, 0, 2, 1, 2, 2, 0, 0, 1, 3, 3],
            Some(&kinds(&["calls", "owns"], &[1, 0, 1, 2, 1, 1])),
        )
    }

    #[test]
    fn neighbours_by_side_in_edge_order() {
        let g = sample();
        assert_eq!(g.neighbors(&0, None, Side::Codomain), vec![1, 2]);
        assert_eq!(g.neighbors(&2, None, Side::Domain), vec![0, 1]);
        assert_eq!(g.neighbors(&0, None, Side::Domain), vec![2]);
        assert_eq!(g.neighbors(&1, None, Side::Codomain), vec![2]);
    }

    #[test]
    fn neighbours_filter_by_kind() {
        let g = sample();
        assert_eq!(g.neighbors(&0, Some(&names(&["calls"])), Side::Codomain), vec![1]);
        assert_eq!(g.neighbors(&0, Some(&names(&["calls", UNTYPED])), Side::Codomain), vec![1, 2]);
        assert_eq!(g.neighbors(&0, Some(&names(&["owns"])), Side::Domain), vec![2]);
        assert!(g.neighbors(&2, Some(&names(&["owns"])), Side::Domain).is_empty());
        assert!(g.neighbors(&0, Some(&names(&["missing"])), Side::Codomain).is_empty());
        assert!(g.neighbors(&0, Some(&[]), Side::Codomain).is_empty());
    }

    #[test]
    fn untyped_slot_zero_is_the_untyped_kind() {
        let g = sample();
        assert_eq!(g.neighbors(&0, Some(&names(&[UNTYPED])), Side::Codomain), vec![2]);
        assert_eq!(g.neighbors(&2, Some(&names(&[UNTYPED])), Side::Domain), vec![0]);
    }

    #[test]
    fn without_kind_data_every_edge_is_untyped() {
        let edges = [0, 1, 1, 2];
        let none = GraphIndex::build(&ids(&["a", "b", "c"]), &edges, None);
        assert!(!none.is_typed());
        assert_eq!(none.neighbors(&0, Some(&names(&[UNTYPED])), Side::Codomain), vec![1]);

        // A slot buffer that does not parallel the edges is ignored whole.
        let skewed = GraphIndex::build(&ids(&["a", "b", "c"]), &edges, Some(&kinds(&["calls"], &[1])));
        assert!(!skewed.is_typed());
        assert!(skewed.neighbors(&0, Some(&names(&["calls"])), Side::Codomain).is_empty());
        assert_eq!(skewed.neighbors(&0, None, Side::Codomain), vec![1]);
    }

    #[test]
    fn duplicates_loops_and_bad_ids_are_dropped() {
        let g = sample();
        assert_eq!(g.neighbors(&0, None, Side::Codomain), vec![1, 2], "a->b listed twice");
        assert!(g.neighbors(&3, None, Side::Codomain).is_empty(), "self-loop");
        assert!(g.neighbors(&3, None, Side::Domain).is_empty(), "self-loop");
        assert!(g.neighbors(&99, None, Side::Codomain).is_empty(), "unknown node");

        let oob = GraphIndex::build(&ids(&["a", "b"]), &[0, 1, 1, 7], None);
        assert_eq!(oob.neighbors(&0, None, Side::Codomain), vec![1]);
        assert!(oob.neighbors(&1, None, Side::Codomain).is_empty());
    }

    #[test]
    fn same_neighbour_under_two_kinds_survives_a_kind_filter() {
        let g = GraphIndex::build(
            &ids(&["a", "b"]),
            &[0, 1, 0, 1],
            Some(&kinds(&["x", "y"], &[1, 2])),
        );
        assert_eq!(g.neighbors(&0, None, Side::Codomain), vec![1]);
        assert_eq!(g.neighbors(&0, Some(&names(&["y"])), Side::Codomain), vec![1]);
    }

    #[test]
    fn adjacent_reports_direction_sorted() {
        let g = sample();
        // a->b out only; a->c and c->a both.
        assert_eq!(g.adjacent(0), vec![(1, true, false), (2, true, true)]);
        assert_eq!(g.adjacent(2), vec![(0, true, true), (1, false, true)]);
        assert!(g.adjacent(3).is_empty());
        assert!(g.adjacent(99).is_empty());
    }

    #[test]
    fn search_is_case_insensitive_substring_in_node_order() {
        let g = GraphIndex::build(&ids(&["Notes/Alpha.md", "notes/beta.md", "Other"]), &[], None);
        assert_eq!(g.search("NOTES"), vec![0, 1]);
        assert_eq!(g.search("alp"), vec![0]);
        assert_eq!(g.search("  oth "), vec![2]);
        assert!(g.search("zzz").is_empty());
        assert!(g.search("").is_empty());
        assert_eq!(g.ids(), vec![0, 1, 2]);
    }

    #[test]
    fn sort_map_is_identity_over_kinds() {
        let g = sample();
        assert_eq!(g.sorts().kinds("calls"), Some(&names(&["calls"])[..]));
        assert_eq!(g.sorts().kinds(UNTYPED), Some(&names(&[UNTYPED])[..]));
        assert_eq!(g.sorts().kinds("any"), None);
        assert_eq!(g.sorts().kinds("missing"), None);
    }

    #[test]
    fn cursor_walks_the_index_through_the_sort_map() {
        let g = sample();
        let mut cur = GraphCursor::<u32>::default();
        cur.select(1); // b
        // h = step toward targets over any kind: b -> c
        let step = NavVerb::Step { sort: "any".into(), side: Side::Codomain };
        assert_eq!(cur.apply(&g, g.sorts(), &step, None), NavEffect::Moved);
        assert_eq!(cur.current, Some(2));
        // l = step toward sources: c -> a (first edge into c)
        let back = NavVerb::Step { sort: "any".into(), side: Side::Domain };
        assert_eq!(cur.apply(&g, g.sorts(), &back, None), NavEffect::Moved);
        assert_eq!(cur.current, Some(0));
        // a typed sort only sees its own kind: no `owns` edge leaves a.
        let owns = NavVerb::Step { sort: "owns".into(), side: Side::Codomain };
        assert!(matches!(cur.apply(&g, g.sorts(), &owns, None), NavEffect::NoTarget(_)));
        // the `calls` sort reaches b from a, the untyped sort reaches c.
        let calls = NavVerb::Step { sort: "calls".into(), side: Side::Codomain };
        assert_eq!(cur.apply(&g, g.sorts(), &calls, None), NavEffect::Moved);
        assert_eq!(cur.current, Some(1));
        cur.select(0);
        let untyped = NavVerb::Step { sort: UNTYPED.into(), side: Side::Codomain };
        assert_eq!(cur.apply(&g, g.sorts(), &untyped, None), NavEffect::Moved);
        assert_eq!(cur.current, Some(2));
    }

    #[test]
    fn siblings_share_a_codomain_neighbour_and_wrap() {
        // x, y, z all point at p (node 0); w points elsewhere.
        let g = GraphIndex::build(&ids(&["p", "x", "y", "z", "w", "q"]), &[1, 0, 2, 0, 3, 0, 4, 5], None);
        let sib = |delta| NavVerb::Sibling { sort: "any".into(), side: Side::Codomain, delta };
        let mut cur = GraphCursor::<u32>::default();
        cur.select(1);
        assert_eq!(cur.apply(&g, g.sorts(), &sib(1), None), NavEffect::Moved);
        assert_eq!(cur.current, Some(2));
        assert_eq!(cur.apply(&g, g.sorts(), &sib(1), Some(2)), NavEffect::Moved);
        assert_eq!(cur.current, Some(1), "two more steps from y wrap past z to x");
        assert_eq!(cur.apply(&g, g.sorts(), &sib(-1), None), NavEffect::Moved);
        assert_eq!(cur.current, Some(3), "k from x wraps to z");
        // p has no codomain neighbour, so it has no siblings.
        cur.select(0);
        assert!(matches!(cur.apply(&g, g.sorts(), &sib(1), None), NavEffect::NoTarget(_)));
    }

    #[test]
    fn intent_parses_nav_and_app_verbs() {
        assert_eq!(
            Intent::parse_intent("nav.step any codomain"),
            Ok(Intent::Nav(NavVerb::Step { sort: "any".into(), side: Side::Codomain }))
        );
        assert_eq!(Intent::parse_intent("nav.first"), Ok(Intent::Nav(NavVerb::First)));
        assert_eq!(Intent::parse_intent("nav.mark q"), Ok(Intent::Nav(NavVerb::Mark('q'))));
        for (text, verb) in [
            ("app.center", AppVerb::Center),
            ("app.fit", AppVerb::Fit),
            ("app.open", AppVerb::Open),
            ("app.inspect", AppVerb::Inspect),
            ("app.toggle_follow", AppVerb::ToggleFollow),
            ("app.which_key", AppVerb::WhichKey),
        ] {
            assert_eq!(Intent::parse_intent(text), Ok(Intent::App(verb)), "{text}");
        }
    }

    #[test]
    fn intent_rejects_unknown_verbs() {
        assert!(Intent::parse_intent("").is_err());
        assert!(Intent::parse_intent("bogus").is_err());
        assert!(Intent::parse_intent("app.nope").is_err());
        assert!(Intent::parse_intent("app.center now").is_err());
        assert!(Intent::parse_intent("nav.step any sideways").is_err());
        assert!(Intent::parse_intent("nav.mark Q").is_err());
        // A nav.* failure reports the nav reason, not "unknown app intent".
        let err = Intent::parse_intent("nav.step any sideways").unwrap_err();
        assert!(err.contains("sideways"), "{err}");
    }

    fn chord(key: Key) -> KeyChord {
        KeyChord { key, shift: false, alt: false, ctrl: false, meta: false }
    }

    fn press(map: &Keymap<Intent>, st: &mut KeymapState, c: char) -> Resolved<Intent> {
        map.feed(st, chord(Key::Char(c)))
    }

    #[test]
    fn default_keymap_binds_the_documented_keys() {
        let map = &*KEYMAP;
        let mut st = KeymapState::default();
        let fired = |r: Resolved<Intent>| match r {
            Resolved::Intent { intent, count } => (intent, count),
            other => panic!("expected intent, got {other:?}"),
        };
        let nav = |sort: &str, side, delta| Intent::Nav(NavVerb::Sibling { sort: sort.into(), side, delta });

        assert_eq!(fired(press(map, &mut st, 'j')).0, nav("any", Side::Codomain, 1));
        assert_eq!(fired(press(map, &mut st, 'k')).0, nav("any", Side::Codomain, -1));
        assert_eq!(
            fired(press(map, &mut st, 'h')).0,
            Intent::Nav(NavVerb::Step { sort: "any".into(), side: Side::Codomain })
        );
        assert_eq!(
            fired(press(map, &mut st, 'l')).0,
            Intent::Nav(NavVerb::Step { sort: "any".into(), side: Side::Domain })
        );
        assert_eq!(fired(press(map, &mut st, 'G')).0, Intent::Nav(NavVerb::Last));
        assert_eq!(fired(press(map, &mut st, '/')).0, Intent::Nav(NavVerb::Search));
        assert_eq!(fired(press(map, &mut st, 'n')).0, Intent::Nav(NavVerb::Match { delta: 1 }));
        assert_eq!(fired(press(map, &mut st, 'N')).0, Intent::Nav(NavVerb::Match { delta: -1 }));
        assert_eq!(fired(press(map, &mut st, '[')).0, Intent::Nav(NavVerb::Cycle { delta: -1 }));
        assert_eq!(fired(press(map, &mut st, ']')).0, Intent::Nav(NavVerb::Cycle { delta: 1 }));
        assert_eq!(fired(press(map, &mut st, 'i')).0, Intent::App(AppVerb::Inspect));
        assert_eq!(fired(press(map, &mut st, '?')).0, Intent::App(AppVerb::WhichKey));
        assert_eq!(
            fired(map.feed(&mut st, chord(Key::Enter))).0,
            Intent::App(AppVerb::Open)
        );
    }

    #[test]
    fn default_keymap_sequences_counts_marks_and_jumplist() {
        let map = &*KEYMAP;
        let mut st = KeymapState::default();

        assert!(matches!(press(map, &mut st, 'g'), Resolved::Pending { .. }));
        assert!(matches!(
            press(map, &mut st, 'g'),
            Resolved::Intent { intent: Intent::Nav(NavVerb::First), .. }
        ));

        assert!(matches!(press(map, &mut st, 'z'), Resolved::Pending { .. }));
        assert!(matches!(
            press(map, &mut st, 'z'),
            Resolved::Intent { intent: Intent::App(AppVerb::Center), .. }
        ));
        press(map, &mut st, 'z');
        assert!(matches!(
            press(map, &mut st, 'f'),
            Resolved::Intent { intent: Intent::App(AppVerb::Fit), .. }
        ));
        press(map, &mut st, 'z');
        assert!(matches!(
            press(map, &mut st, 't'),
            Resolved::Intent { intent: Intent::App(AppVerb::ToggleFollow), .. }
        ));

        assert!(matches!(press(map, &mut st, 'm'), Resolved::Pending { .. }));
        assert!(matches!(
            press(map, &mut st, 'q'),
            Resolved::Intent { intent: Intent::Nav(NavVerb::Mark('q')), .. }
        ));
        assert!(matches!(press(map, &mut st, '\''), Resolved::Pending { .. }));
        assert!(matches!(
            press(map, &mut st, 'q'),
            Resolved::Intent { intent: Intent::Nav(NavVerb::Jump('q')), .. }
        ));

        let ctrl = |c| KeyChord { ctrl: true, ..chord(Key::Char(c)) };
        assert!(matches!(
            map.feed(&mut st, ctrl('o')),
            Resolved::Intent { intent: Intent::Nav(NavVerb::JumpList { delta: -1 }), .. }
        ));
        assert!(matches!(
            map.feed(&mut st, ctrl('i')),
            Resolved::Intent { intent: Intent::Nav(NavVerb::JumpList { delta: 1 }), .. }
        ));

        // A count prefixes the verb.
        assert!(matches!(press(map, &mut st, '3'), Resolved::Pending { .. }));
        assert!(matches!(
            press(map, &mut st, 'j'),
            Resolved::Intent { count: Some(3), .. }
        ));
    }

    #[test]
    fn default_keymap_leaves_camera_and_panel_keys_to_the_host() {
        let map = &*KEYMAP;
        let mut st = KeymapState::default();
        for c in ['w', 'a', 's', 'd', 'q', 'e', 'f', 'c', 't'] {
            assert!(
                matches!(press(map, &mut st, c), Resolved::Unbound(_)),
                "`{c}` must fall through to the camera/panel handlers"
            );
        }
        assert!(matches!(map.feed(&mut st, chord(Key::Escape)), Resolved::Unbound(_)));
        press(map, &mut st, 'g');
        assert_eq!(map.feed(&mut st, chord(Key::Escape)), Resolved::Cancelled);
    }
}
