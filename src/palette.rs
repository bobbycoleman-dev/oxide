//! Command palette matching: a small fuzzy scorer over the action registry.
//!
//! Pure functions, no GPUI, so ranking is unit-testable. The app owns the
//! palette's state and rendering.

use crate::keymap::registry::ActionMeta;

/// One row in the palette's filtered list.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteItem {
    pub action_id: &'static str,
    pub title: &'static str,
    pub category: &'static str,
    /// Char positions in `title` that matched, for highlight rendering.
    pub highlights: Vec<usize>,
    /// Keystrokes to show, already pretty-printed.
    pub binding: Option<String>,
    pub score: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub score: i32,
    pub positions: Vec<usize>,
}

const MATCH: i32 = 10;
const CONSECUTIVE: i32 = 8;
const WORD_START: i32 = 6;
const GAP: i32 = 1;

fn is_word_start(chars: &[char], ix: usize) -> bool {
    ix == 0 || !chars[ix - 1].is_alphanumeric()
}

/// Case-insensitive subsequence match with the alignment that scores best:
/// consecutive runs beat scattered hits, word-initial hits beat mid-word
/// ones, and every skipped character between hits costs a little.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<Match> {
    let q: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).filter(|c| !c.is_whitespace()).collect();
    let c: Vec<char> = candidate.chars().collect();
    let cl: Vec<char> = c.iter().flat_map(|ch| ch.to_lowercase().next()).collect();
    if q.is_empty() {
        return Some(Match { score: 0, positions: Vec::new() });
    }
    if q.len() > cl.len() {
        return None;
    }
    let (n, m) = (q.len(), cl.len());
    // best[i][j]: best score with q[i] matched at c[j]; prev[i][j]: the j
    // chosen for q[i-1] in that alignment.
    let mut best = vec![vec![i32::MIN; m]; n];
    let mut prev = vec![vec![usize::MAX; m]; n];
    for j in 0..m {
        if cl[j] == q[0] {
            best[0][j] = MATCH + if is_word_start(&c, j) { WORD_START } else { 0 } - (j as i32).min(3) * 0;
        }
    }
    for i in 1..n {
        for j in i..m {
            if cl[j] != q[i] {
                continue;
            }
            let here = MATCH + if is_word_start(&c, j) { WORD_START } else { 0 };
            let mut top = i32::MIN;
            let mut from = usize::MAX;
            for k in (i - 1)..j {
                if best[i - 1][k] == i32::MIN {
                    continue;
                }
                let gap = (j - k - 1) as i32;
                let s = best[i - 1][k] + here + if gap == 0 { CONSECUTIVE } else { -gap * GAP };
                if s > top {
                    top = s;
                    from = k;
                }
            }
            if top != i32::MIN {
                best[i][j] = top;
                prev[i][j] = from;
            }
        }
    }
    let (mut j, score) = (0..m)
        .filter(|&j| best[n - 1][j] != i32::MIN)
        .map(|j| (j, best[n - 1][j]))
        .max_by_key(|&(j, s)| (s, std::cmp::Reverse(j)))?;
    let mut positions = vec![0; n];
    for i in (0..n).rev() {
        positions[i] = j;
        if i > 0 {
            j = prev[i][j];
        }
    }
    // Shorter candidates win ties: a match covering more of the title is a
    // better answer to the same query.
    Some(Match { score: score - (m as i32) / 8, positions })
}

/// Score an action against a query across its title, category, and aliases.
/// Only a title match yields highlight positions.
pub fn score_action(query: &str, meta: &ActionMeta) -> Option<Match> {
    let mut best = fuzzy_match(query, meta.title);
    let mut consider = |text: &str, penalty: i32| {
        if let Some(m) = fuzzy_match(query, text) {
            let alt = Match { score: m.score - penalty, positions: Vec::new() };
            if best.as_ref().is_none_or(|b| alt.score > b.score) {
                best = Some(alt);
            }
        }
    };
    consider(&format!("{} {}", meta.category, meta.title), 3);
    for alias in meta.aliases {
        consider(alias, 2);
    }
    best
}

/// Build the palette's rows: with a query, every candidate that matches,
/// best first; without one, most recently used first, then registry order.
pub fn build_items<'a>(
    query: &str,
    candidates: impl Iterator<Item = &'a ActionMeta>,
    recent: &[&str],
    binding_for: impl Fn(&str) -> Option<String>,
) -> Vec<PaletteItem> {
    let query = query.trim();
    let mut items: Vec<PaletteItem> = candidates
        .filter_map(|meta| {
            let m = score_action(query, meta)?;
            Some(PaletteItem {
                action_id: meta.id,
                title: meta.title,
                category: meta.category,
                highlights: m.positions,
                binding: binding_for(meta.id),
                score: m.score,
            })
        })
        .collect();
    if query.is_empty() {
        let rank = |id: &str| recent.iter().position(|r| *r == id).unwrap_or(usize::MAX);
        items.sort_by_key(|it| rank(it.action_id));
    } else {
        items.sort_by(|a, b| b.score.cmp(&a.score).then(a.title.len().cmp(&b.title.len())));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::registry;

    fn top(query: &str) -> Vec<&'static str> {
        build_items(query, registry::all().iter(), &[], |_| None)
            .into_iter()
            .map(|i| i.action_id)
            .collect()
    }

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_match("spr", "Split Right").is_some());
        assert!(fuzzy_match("xyz", "Split Right").is_none());
        assert!(fuzzy_match("thgir", "Split Right").is_none(), "order matters");
        assert_eq!(fuzzy_match("", "anything").unwrap().positions, Vec::<usize>::new());
    }

    #[test]
    fn highlights_point_at_matched_chars() {
        let m = fuzzy_match("spr", "Split Right").unwrap();
        assert_eq!(m.positions, vec![0, 1, 6]);
        let m = fuzzy_match("SR", "split right").unwrap();
        assert_eq!(m.positions, vec![0, 6], "case-insensitive");
    }

    #[test]
    fn consecutive_beats_scattered_and_word_starts_beat_mid_word() {
        let run = fuzzy_match("spl", "Split Right").unwrap().score;
        let scattered = fuzzy_match("spl", "Select Pane Left-ish").unwrap().score;
        assert!(run > scattered, "{run} vs {scattered}");
        let boundary = fuzzy_match("fr", "Font Reset").unwrap().score;
        let mid = fuzzy_match("fr", "Buffer Ram").unwrap().score;
        assert!(boundary > mid, "{boundary} vs {mid}");
    }

    #[test]
    fn shorter_titles_win_ties() {
        let short = fuzzy_match("tab", "New Tab").unwrap().score;
        let long = fuzzy_match("tab", "New Tab In A Very Long Title").unwrap().score;
        assert!(short > long);
    }

    #[test]
    fn spr_ranks_split_right_first() {
        let ranked = top("spr");
        assert_eq!(ranked[0], "pane::split_right", "{ranked:?}");
        // "Workspace › Workspaces: Pin / Unpin" is a scattered match at best.
        let pin = ranked.iter().position(|id| *id == "workspace::toggle_persist");
        assert!(pin.is_none_or(|ix| ix > 3), "{ranked:?}");
        assert_eq!(top("pal")[0], "app::palette");
        assert_eq!(top("theme")[0], "app::select_theme");
        assert_eq!(top("eq")[0], "pane::equalize");
    }

    #[test]
    fn aliases_and_categories_match_without_highlights() {
        let items = build_items("vsplit", registry::all().iter(), &[], |_| None);
        let ids: Vec<_> = items.iter().map(|i| i.action_id).collect();
        // Both vertical splits carry the alias; nothing else should outrank them.
        assert!(ids[..2].contains(&"pane::split_right") && ids[..2].contains(&"pane::split_left"), "{ids:?}");
        assert!(items[0].highlights.is_empty());
        let items = build_items("terminal copy", registry::all().iter(), &[], |_| None);
        assert_eq!(items[0].action_id, "terminal::copy");
    }

    #[test]
    fn empty_query_lists_recent_first_then_registry_order() {
        let items = build_items("", registry::all().iter(), &["tab::new", "app::quit"], |_| None);
        assert_eq!(items[0].action_id, "tab::new");
        assert_eq!(items[1].action_id, "app::quit");
        assert_eq!(items.len(), registry::all().len());
        let rest: Vec<_> = items[2..].iter().map(|i| i.action_id).collect();
        let expected: Vec<_> = registry::all()
            .iter()
            .map(|m| m.id)
            .filter(|id| *id != "tab::new" && *id != "app::quit")
            .collect();
        assert_eq!(rest, expected);
    }

    #[test]
    fn bindings_are_attached() {
        let items = build_items("quit", registry::all().iter(), &[], |id| (id == "app::quit").then(|| "⌘Q".to_string()));
        assert_eq!(items[0].binding.as_deref(), Some("⌘Q"));
    }
}
