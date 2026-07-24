//! The `:` command palette — k9s-style resource jumping with live filtering
//! and autocompletion.
//!
//! Candidates come from the cluster's own discovery API, so the palette only
//! ever offers kinds this cluster actually serves — CRDs included.

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

/// One thing the palette can complete to.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// What is displayed, and what gets accepted on `Enter`.
    pub value: String,
    /// Extra strings that also select this candidate — for resource types,
    /// the singular kind and the kubectl short name. Without these, typing a
    /// short name like `pvc` would fuzzy-match some unrelated plural
    /// (`apiservices` contains p, v and c in order) instead of the kind the
    /// user meant.
    pub keys: Vec<String>,
}

impl Candidate {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            keys: Vec::new(),
        }
    }

    pub fn with_keys(value: impl Into<String>, keys: Vec<String>) -> Self {
        Self {
            value: value.into(),
            keys,
        }
    }

    /// Best score across the displayed value and every alias.
    fn score(&self, query: &str) -> Option<Score> {
        std::iter::once(&self.value)
            .chain(&self.keys)
            .filter_map(|k| score(k, query))
            .max()
    }
}

/// State of the palette while it is open.
#[derive(Debug, Default)]
pub struct Palette {
    /// Everything the palette could complete to.
    pub candidates: Vec<Candidate>,
    /// Values matching the current input, best match first.
    pub matches: Vec<String>,
    /// Index into `matches`.
    pub selected: usize,
}

/// How many completions the dropdown shows at once.
pub const MAX_VISIBLE: usize = 8;

impl Palette {
    /// Replace the candidate set and re-rank against `query`.
    pub fn reload(&mut self, candidates: Vec<Candidate>, query: &str) {
        self.candidates = candidates;
        self.refilter(query);
    }

    /// Re-rank the candidates against the current input.
    pub fn refilter(&mut self, query: &str) {
        let needle = query.trim().to_ascii_lowercase();
        let mut scored: Vec<(Score, &str)> = self
            .candidates
            .iter()
            .filter_map(|c| c.score(&needle).map(|s| (s, c.value.as_str())))
            .collect();
        // Descending by score, then alphabetically so the list is stable
        // between keystrokes instead of jittering on equal scores.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        self.matches = scored.into_iter().map(|(_, c)| c.to_string()).collect();
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
        p.reload(items.iter().map(|s| Candidate::new(*s)).collect(), query);
        p.matches
    }

    fn keyed(items: &[(&str, &[&str])], query: &str) -> Vec<String> {
        let mut p = Palette::default();
        p.reload(
            items
                .iter()
                .map(|(v, keys)| {
                    Candidate::with_keys(*v, keys.iter().map(|k| k.to_string()).collect())
                })
                .collect(),
            query,
        );
        p.matches
    }

    #[test]
    fn a_short_name_beats_an_accidental_subsequence() {
        // "pvc" is a subsequence of "apiservices" (a-P-iser-V-i-C-es), so
        // without the alias the wrong kind would win.
        let got = keyed(
            &[
                ("apiservices", &[]),
                ("persistentvolumeclaims", &["persistentvolumeclaim", "pvc"]),
            ],
            "pvc",
        );
        assert_eq!(got.first().unwrap(), "persistentvolumeclaims");
    }

    #[test]
    fn aliases_match_but_the_display_value_is_returned() {
        let got = keyed(&[("pods", &["pod", "po"])], "po");
        assert_eq!(got, vec!["pods"]);
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
        p.reload(vec![Candidate::new("a"), Candidate::new("b")], "");
        p.move_selection(-1);
        assert_eq!(p.selected, 1);
        p.move_selection(1);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn visible_window_follows_the_selection() {
        let mut p = Palette::default();
        p.reload(
            (0..20)
                .map(|i| Candidate::new(format!("item{i:02}")))
                .collect(),
            "",
        );
        p.selected = 12;
        let (start, rows) = p.visible();
        assert_eq!(rows.len(), MAX_VISIBLE);
        assert!(start <= 12 && 12 < start + MAX_VISIBLE);
    }

    #[test]
    fn visible_window_is_the_whole_list_when_it_fits() {
        let mut p = Palette::default();
        p.reload(vec![Candidate::new("a"), Candidate::new("b")], "");
        assert_eq!(p.visible(), (0, &["a".to_string(), "b".to_string()][..]));
    }
}
