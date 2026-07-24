//! The `:` command palette — k9s-style resource jumping with live filtering
//! and autocompletion.
//!
//! Candidates are derived from whatever is actually on screen (the kinds and
//! `kind/name` pairs present in the current event list), so the palette never
//! offers a resource the cluster has nothing to say about.

/// How well a candidate matched, best first. Ordering matters more than the
/// absolute numbers: an exact prefix always beats a substring, which always
/// beats a scattered subsequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Score {
    /// Characters appear in order but not adjacently ("dpy" → "deployment").
    Subsequence(std::cmp::Reverse<usize>),
    /// The query appears verbatim somewhere inside.
    Contains(std::cmp::Reverse<usize>),
    /// The candidate starts with the query.
    Prefix(std::cmp::Reverse<usize>),
    /// The candidate *is* the query.
    Exact,
}

/// Rank `candidate` against `query`, or `None` when it does not match at all.
/// Both are compared case-insensitively; `query` is assumed already lowercased.
fn score(candidate: &str, query: &str) -> Option<Score> {
    let lower = candidate.to_ascii_lowercase();
    if query.is_empty() {
        return Some(Score::Subsequence(std::cmp::Reverse(lower.len())));
    }
    if lower == query {
        return Some(Score::Exact);
    }
    // Shorter candidates win ties: "pod" should outrank "pod/some-long-name".
    let brevity = std::cmp::Reverse(lower.len());
    if lower.starts_with(query) {
        return Some(Score::Prefix(brevity));
    }
    if lower.contains(query) {
        return Some(Score::Contains(brevity));
    }
    let mut haystack = lower.chars();
    if query.chars().all(|c| haystack.any(|h| h == c)) {
        return Some(Score::Subsequence(brevity));
    }
    None
}

/// State of the palette while it is open.
#[derive(Debug, Default)]
pub struct Palette {
    /// Everything the palette could complete to.
    pub candidates: Vec<String>,
    /// Candidates matching the current input, best match first.
    pub matches: Vec<String>,
    /// Index into `matches`.
    pub selected: usize,
}

/// How many completions the dropdown shows at once.
pub const MAX_VISIBLE: usize = 8;

impl Palette {
    /// Replace the candidate set and re-rank against `query`.
    pub fn reload(&mut self, candidates: Vec<String>, query: &str) {
        self.candidates = candidates;
        self.refilter(query);
    }

    /// Re-rank the candidates against the current input.
    pub fn refilter(&mut self, query: &str) {
        let needle = query.trim().to_ascii_lowercase();
        let mut scored: Vec<(Score, &String)> = self
            .candidates
            .iter()
            .filter_map(|c| score(c, &needle).map(|s| (s, c)))
            .collect();
        // Descending by score, then alphabetically so the list is stable
        // between keystrokes instead of jittering on equal scores.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        self.matches = scored.into_iter().map(|(_, c)| c.clone()).collect();
        self.selected = 0;
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        // Wrap around, so Tab cycles the way k9s does.
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    /// The completion `Tab`/`Enter` would accept.
    pub fn current(&self) -> Option<&str> {
        self.matches.get(self.selected).map(String::as_str)
    }

    /// Window of matches to render, and the offset it starts at, so the
    /// selected row is always visible in a `MAX_VISIBLE`-tall dropdown.
    pub fn visible(&self) -> (usize, &[String]) {
        if self.matches.len() <= MAX_VISIBLE {
            return (0, &self.matches);
        }
        let start = self
            .selected
            .saturating_sub(MAX_VISIBLE - 1)
            .min(self.matches.len() - MAX_VISIBLE);
        (start, &self.matches[start..start + MAX_VISIBLE])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette(items: &[&str], query: &str) -> Vec<String> {
        let mut p = Palette::default();
        p.reload(items.iter().map(|s| s.to_string()).collect(), query);
        p.matches
    }

    #[test]
    fn exact_beats_prefix_beats_contains_beats_subsequence() {
        let got = palette(&["mypod", "pod/api", "pod", "pxod"], "pod");
        assert_eq!(got, vec!["pod", "pod/api", "mypod", "pxod"]);
    }

    #[test]
    fn shorter_candidates_win_ties() {
        let got = palette(&["pod/a-very-long-name", "pod/ab"], "pod/a");
        assert_eq!(got, vec!["pod/ab", "pod/a-very-long-name"]);
    }

    #[test]
    fn matches_a_scattered_subsequence() {
        assert_eq!(palette(&["deployment"], "dpy"), vec!["deployment"]);
        assert!(palette(&["deployment"], "zzz").is_empty());
    }

    #[test]
    fn matching_ignores_case_on_both_sides() {
        assert_eq!(palette(&["Pod/API"], "pod/api"), vec!["Pod/API"]);
    }

    #[test]
    fn empty_query_keeps_everything() {
        assert_eq!(palette(&["a", "b"], "").len(), 2);
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut p = Palette::default();
        p.reload(vec!["a".into(), "b".into()], "");
        p.move_selection(-1);
        assert_eq!(p.selected, 1);
        p.move_selection(1);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn visible_window_follows_the_selection() {
        let mut p = Palette::default();
        p.reload((0..20).map(|i| format!("item{i:02}")).collect(), "");
        p.selected = 12;
        let (start, rows) = p.visible();
        assert_eq!(rows.len(), MAX_VISIBLE);
        assert!(start <= 12 && 12 < start + MAX_VISIBLE);
    }

    #[test]
    fn visible_window_is_the_whole_list_when_it_fits() {
        let mut p = Palette::default();
        p.reload(vec!["a".into(), "b".into()], "");
        assert_eq!(p.visible(), (0, &["a".to_string(), "b".to_string()][..]));
    }
}
