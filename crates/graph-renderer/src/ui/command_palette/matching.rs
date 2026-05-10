//! Pure ranking + highlight helpers for the command palette.
//!
//! Split out so the renderer in `command_palette/mod.rs` only deals
//! with egui state, while the matching logic — fuzzy scoring against
//! actions, fzf-style file matches, hit-aware text highlighting — is
//! testable in isolation.

use eframe::egui;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::proto::NodeMeta;
use crate::ui::actions::Action;
use crate::ui::theme::{accent, palette};

/// Maximum number of file/node matches surfaced under the action list.
pub(super) const FILE_MATCH_LIMIT: usize = 50;

#[derive(Debug, Clone, Default)]
pub(super) struct MatchInfo {
    pub score: i32,
    /// Byte indices in `title` that matched (used for highlighting).
    pub title_hits: Vec<usize>,
}

/// Case-insensitive subsequence match across title + description + keywords.
/// Returns `None` if every character in the query couldn't be placed.
/// Score rewards consecutive matches and earlier positions.
pub(super) fn fuzzy_score(action: &Action, query: &str) -> Option<MatchInfo> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return Some(MatchInfo::default());
    }
    // Title is the primary haystack; description + keywords broaden hits.
    let title_lc = action.title.to_lowercase();
    let mut title_hits: Vec<usize> = Vec::new();
    let title_score = subsequence_score(&title_lc, &q, Some(&mut title_hits));

    let extras = format!(
        "{} {} {}",
        action.description.to_lowercase(),
        action.keywords.join(" ").to_lowercase(),
        action.id.to_lowercase()
    );
    let extra_score = subsequence_score(&extras, &q, None);

    if title_score.is_none() && extra_score.is_none() {
        return None;
    }
    let title = title_score.unwrap_or(0);
    let extra = extra_score.unwrap_or(0);
    Some(MatchInfo {
        // Title matches weighted 3x — name typing should dominate.
        score: title * 3 + extra,
        title_hits,
    })
}

fn subsequence_score(
    haystack: &str,
    needle: &str,
    mut hits: Option<&mut Vec<usize>>,
) -> Option<i32> {
    let mut score: i32 = 0;
    let mut last_match: Option<usize> = None;
    let mut hi = 0;
    let h_bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    let mut ni = 0;
    while ni < n_bytes.len() {
        let nb = n_bytes[ni];
        let mut found = None;
        while hi < h_bytes.len() {
            if h_bytes[hi] == nb {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        let pos = found?;
        if let Some(h) = hits.as_deref_mut() {
            h.push(pos);
        }
        // Reward consecutive hits, prefer earlier matches.
        if Some(pos.wrapping_sub(1)) == last_match {
            score += 5;
        } else {
            score += 1;
        }
        if pos < 8 {
            score += 2;
        }
        last_match = Some(pos);
        ni += 1;
    }
    Some(score)
}

/// One ranked vault node + the byte indices in its id that matched.
#[derive(Debug, Clone)]
pub(super) struct FileMatch {
    pub id: String,
    pub score: i64,
    pub indices: Vec<usize>,
}

pub(super) fn rank_files(query: &str, nodes: &[String]) -> Vec<FileMatch> {
    if query.trim().is_empty() || nodes.is_empty() {
        return Vec::new();
    }
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut scored: Vec<FileMatch> = nodes
        .iter()
        .filter_map(|id| {
            matcher
                .fuzzy_indices(id, query)
                .map(|(score, indices)| FileMatch {
                    id: id.clone(),
                    score,
                    indices,
                })
        })
        .collect();
    scored.sort_by(|a, b| b.score.cmp(&a.score));
    scored.truncate(FILE_MATCH_LIMIT);
    scored
}

/// Synthesize a previewable "document" from a NodeMeta. We don't yet
/// have a file-body endpoint (`/node/:id` only returns metadata), so
/// the document viewer renders the metadata as YAML-ish text.
pub(super) fn preview_text_for(meta: &NodeMeta) -> String {
    let mut s = String::new();
    s.push_str(&format!("id:        {}\n", meta.id));
    s.push_str(&format!("title:     {}\n", meta.title));
    s.push_str(&format!("path:      {}\n", meta.path));
    s.push_str(&format!("folder:    {}\n", meta.folder));
    if let Some(dt) = &meta.doctype {
        s.push_str(&format!("doctype:   {}\n", dt));
    }
    if !meta.tags.is_empty() {
        s.push_str(&format!("tags:      [{}]\n", meta.tags.join(", ")));
    }
    s.push_str("\n# metrics\n");
    s.push_str(&format!("degree:    {}\n", meta.degree));
    s.push_str(&format!("indegree:  {}\n", meta.indegree));
    s.push_str(&format!("outdegree: {}\n", meta.outdegree));
    s.push_str(&format!("pagerank:  {:.6}\n", meta.pagerank));
    s.push_str(&format!("kcore:     {}\n", meta.kcore));
    s.push_str(&format!("community: {}\n", meta.community));
    s.push_str(&format!("wcc:       {}\n", meta.wcc));
    if !meta.frontmatter_json.is_empty() && meta.frontmatter_json != "{}" {
        s.push_str("\n# frontmatter\n");
        match serde_json::from_str::<serde_json::Value>(&meta.frontmatter_json) {
            Ok(v) => match serde_json::to_string_pretty(&v) {
                Ok(pp) => s.push_str(&pp),
                Err(_) => s.push_str(&meta.frontmatter_json),
            },
            Err(_) => s.push_str(&meta.frontmatter_json),
        }
    }
    s
}

pub(super) fn highlighted_title(
    title: &str,
    hits: &[usize],
    focused: bool,
) -> egui::WidgetText {
    // Focused row keeps WHITE (max contrast on the focused row's tinted
    // bg). Non-focused rows pick the body TEXT colour so the list
    // doesn't read as a wall of LED-on-black.
    let base = if focused { egui::Color32::WHITE } else { palette::TEXT };
    if hits.is_empty() {
        return egui::RichText::new(title).color(base).into();
    }
    use egui::text::LayoutJob;
    let mut job = LayoutJob::default();
    let bytes = title.as_bytes();
    let mut i = 0;
    let in_hits = |idx: usize| hits.contains(&idx);
    while i < bytes.len() {
        let start = i;
        let hit_now = in_hits(i);
        while i < bytes.len() && in_hits(i) == hit_now {
            i += 1;
        }
        let chunk = &title[start..i];
        let mut fmt = egui::TextFormat::default();
        if hit_now {
            fmt.color = accent::BLUE;
            fmt.font_id = crate::ui::theme::mono(crate::ui::theme::font_size::BODY);
        } else {
            fmt.color = base;
            fmt.font_id = crate::ui::theme::mono(crate::ui::theme::font_size::BODY);
        }
        job.append(chunk, 0.0, fmt);
    }
    egui::WidgetText::LayoutJob(job)
}

pub(super) fn highlighted_path(
    path: &str,
    hits: &[usize],
    _name_len: usize,
    focused: bool,
) -> egui::WidgetText {
    // Same focus-aware contrast rule as highlighted_title.
    let base = if focused { egui::Color32::WHITE } else { palette::TEXT };
    use egui::text::LayoutJob;
    let mut job = LayoutJob::default();
    let bytes = path.as_bytes();
    let mut i = 0usize;
    let in_hits = |idx: usize| hits.iter().any(|&h| h == idx);
    while i < bytes.len() {
        let start = i;
        let hit_now = in_hits(i);
        while i < bytes.len() && in_hits(i) == hit_now {
            i += 1;
        }
        let chunk = &path[start..i];
        let mut fmt = egui::TextFormat::default();
        fmt.font_id = crate::ui::theme::mono(crate::ui::theme::font_size::BODY);
        fmt.color = if hit_now {
            accent::YELLOW
        } else {
            base
        };
        if hit_now {
            fmt.background = egui::Color32::from_rgba_unmultiplied(0xff, 0xd5, 0x4a, 0x30);
        }
        job.append(chunk, 0.0, fmt);
    }
    egui::WidgetText::LayoutJob(job)
}
