//! Log storage and processing.
//!
//! Design notes (this is the hot path, so it is worth stating explicitly):
//!
//! * Lines live in a fixed-capacity [`VecDeque`] ring buffer. Pushing is O(1)
//!   and never reallocates once the buffer is full.
//! * Every line is classified **once**, on arrival, into a [`Level`]. Filtering
//!   and colouring then only look at a `u8`-sized enum instead of re-scanning
//!   text on every frame.
//! * The filtered "view" is a deque of global line ids, updated incrementally on
//!   push. Only a filter change triggers a full rebuild.
//! * Nothing is styled until it is about to be drawn, and only the visible
//!   window is styled.

use std::collections::VecDeque;
use std::sync::Arc;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use regex::Regex;

use crate::config::Theme;

/// Severity classification of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }

    pub fn style(self, theme: &Theme) -> Style {
        let s = Style::default();
        match self {
            Level::Trace => s.fg(Theme::color(&theme.trace)),
            Level::Debug => s.fg(Theme::color(&theme.debug)),
            Level::Info => s.fg(Theme::color(&theme.fg)),
            Level::Warn => s.fg(Theme::color(&theme.warn)),
            Level::Error => s.fg(Theme::color(&theme.error)),
            Level::Fatal => s
                .fg(Theme::color(&theme.error))
                .add_modifier(Modifier::BOLD),
        }
    }

    /// Cycle through the level filter thresholds with a single key press.
    pub fn next_threshold(current: Level) -> Level {
        match current {
            Level::Trace => Level::Debug,
            Level::Debug => Level::Info,
            Level::Info => Level::Warn,
            Level::Warn => Level::Error,
            Level::Error => Level::Fatal,
            Level::Fatal => Level::Trace,
        }
    }
}

/// Case-insensitive substring search that avoids allocating.
fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let lower = needle[0].to_ascii_lowercase();
    let upper = needle[0].to_ascii_uppercase();
    let last_start = hay.len() - needle.len();
    let mut i = 0usize;
    while i <= last_start {
        match memchr::memchr2(lower, upper, &hay[i..=last_start]) {
            Some(p) => {
                let start = i + p;
                if hay[start..start + needle.len()].eq_ignore_ascii_case(needle) {
                    return Some(start);
                }
                i = start + 1;
            }
            None => return None,
        }
    }
    None
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A standalone (word-delimited) case-insensitive token match.
fn has_token(hay: &[u8], token: &[u8]) -> bool {
    let mut offset = 0usize;
    while let Some(p) = find_ci(&hay[offset..], token) {
        let start = offset + p;
        let end = start + token.len();
        let before_ok = start == 0 || !is_word_byte(hay[start - 1]);
        let after_ok = end == hay.len() || !is_word_byte(hay[end]);
        if before_ok && after_ok {
            return true;
        }
        offset = start + 1;
        if offset >= hay.len() {
            break;
        }
    }
    false
}

/// Classify a raw log line. Only the first 512 bytes are inspected: log
/// severity is virtually always at the front, and this keeps the cost bounded
/// for multi-kilobyte stack traces or JSON blobs.
pub fn classify(raw: &str) -> Level {
    let bytes = raw.as_bytes();
    let head = &bytes[..bytes.len().min(512)];

    for token in [b"fatal".as_ref(), b"panic".as_ref(), b"emerg".as_ref()] {
        if has_token(head, token) {
            return Level::Fatal;
        }
    }
    for token in [b"error".as_ref(), b"err".as_ref(), b"critical".as_ref()] {
        if has_token(head, token) {
            return Level::Error;
        }
    }
    if find_ci(head, b"\"level\":\"error\"").is_some() || find_ci(head, b"exception").is_some() {
        return Level::Error;
    }
    for token in [b"warn".as_ref(), b"warning".as_ref()] {
        if has_token(head, token) {
            return Level::Warn;
        }
    }
    for token in [b"debug".as_ref()] {
        if has_token(head, token) {
            return Level::Debug;
        }
    }
    for token in [b"trace".as_ref()] {
        if has_token(head, token) {
            return Level::Trace;
        }
    }
    Level::Info
}

/// One stored log line.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub raw: Box<str>,
    pub level: Level,
    pub source: Arc<str>,
    /// Byte offset where the message body starts (after a leading RFC3339
    /// timestamp emitted by the API server), or 0.
    pub body_at: u16,
}

impl LogLine {
    pub fn new(raw: String, source: Arc<str>) -> Self {
        let level = classify(&raw);
        let body_at = timestamp_len(&raw);
        Self {
            raw: raw.into_boxed_str(),
            level,
            source,
            body_at,
        }
    }

    pub fn timestamp(&self) -> Option<&str> {
        if self.body_at == 0 {
            None
        } else {
            Some(&self.raw[..self.body_at as usize - 1])
        }
    }

    pub fn body(&self) -> &str {
        &self.raw[self.body_at as usize..]
    }
}

/// Length of a leading RFC3339 timestamp *including* the trailing space, or 0.
fn timestamp_len(raw: &str) -> u16 {
    let b = raw.as_bytes();
    if b.len() < 21 {
        return 0;
    }
    let looks_like_date = b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T';
    if !looks_like_date {
        return 0;
    }
    match memchr::memchr(b' ', &b[..b.len().min(64)]) {
        Some(p) => (p + 1) as u16,
        None => 0,
    }
}

/// Active filter over the buffer.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Minimum severity to display.
    pub min_level: Level,
    /// Only display error/fatal lines regardless of `min_level`.
    pub errors_only: bool,
    /// Optional grep-style include filter.
    pub include: Option<Regex>,
    /// Optional exclude filter, applied after `include`.
    pub exclude: Option<Regex>,
}

impl Filter {
    pub fn is_active(&self) -> bool {
        self.errors_only
            || self.min_level != Level::Trace
            || self.include.is_some()
            || self.exclude.is_some()
    }

    pub fn accepts(&self, line: &LogLine) -> bool {
        if self.errors_only && line.level < Level::Error {
            return false;
        }
        if line.level < self.min_level {
            return false;
        }
        if let Some(re) = &self.include {
            if !re.is_match(&line.raw) {
                return false;
            }
        }
        if let Some(re) = &self.exclude {
            if re.is_match(&line.raw) {
                return false;
            }
        }
        true
    }

    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.errors_only {
            parts.push("errors-only".into());
        } else if self.min_level != Level::Trace {
            parts.push(format!(">={}", self.min_level.label()));
        }
        if let Some(re) = &self.include {
            parts.push(format!("include:/{}/", re.as_str()));
        }
        if let Some(re) = &self.exclude {
            parts.push(format!("exclude:/{}/", re.as_str()));
        }
        if parts.is_empty() {
            "none".into()
        } else {
            parts.join(" ")
        }
    }
}

/// Bounded ring buffer of log lines with an incrementally maintained view.
#[derive(Debug)]
pub struct LogBuffer {
    lines: VecDeque<LogLine>,
    /// Global id of `lines[0]`.
    base: u64,
    cap: usize,
    /// Total lines ever received (including evicted ones).
    pub received: u64,
    /// Lines evicted because the buffer was full.
    pub dropped: u64,
    /// Global ids of lines matching `filter`, in order.
    view: VecDeque<u64>,
    pub filter: Filter,
    /// Per-level counters over the retained window.
    pub counts: [u64; 6],
}

impl LogBuffer {
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(64);
        Self {
            lines: VecDeque::with_capacity(cap.min(8192)),
            base: 0,
            cap,
            received: 0,
            dropped: 0,
            view: VecDeque::new(),
            filter: Filter::default(),
            counts: [0; 6],
        }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Number of lines currently visible under the active filter.
    pub fn view_len(&self) -> usize {
        self.view.len()
    }

    pub fn count_of(&self, level: Level) -> u64 {
        self.counts[level as usize]
    }

    pub fn push(&mut self, line: LogLine) {
        self.received += 1;
        if self.lines.len() == self.cap {
            if let Some(old) = self.lines.pop_front() {
                self.counts[old.level as usize] = self.counts[old.level as usize].saturating_sub(1);
                if self.view.front() == Some(&self.base) {
                    self.view.pop_front();
                }
                self.base += 1;
                self.dropped += 1;
            }
        }
        let id = self.base + self.lines.len() as u64;
        self.counts[line.level as usize] += 1;
        if self.filter.accepts(&line) {
            self.view.push_back(id);
        }
        self.lines.push_back(line);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.view.clear();
        self.counts = [0; 6];
        self.base += self.received - self.dropped;
        self.dropped = 0;
    }

    /// Replace the filter and rebuild the view. O(n) but only on user action.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.rebuild_view();
    }

    fn rebuild_view(&mut self) {
        self.view.clear();
        for (i, line) in self.lines.iter().enumerate() {
            if self.filter.accepts(line) {
                self.view.push_back(self.base + i as u64);
            }
        }
    }

    /// Fetch the nth line of the filtered view.
    pub fn view_line(&self, index: usize) -> Option<&LogLine> {
        let id = *self.view.get(index)?;
        self.lines.get((id - self.base) as usize)
    }

    /// Iterate over a window of the filtered view without allocating.
    pub fn view_range(&self, start: usize, len: usize) -> impl Iterator<Item = &LogLine> {
        let end = (start + len).min(self.view.len());
        let start = start.min(end);
        self.view
            .iter()
            .skip(start)
            .take(end - start)
            .filter_map(move |id| self.lines.get((*id - self.base) as usize))
    }

    /// Find the next view index at or after `from` whose line matches `re`.
    pub fn search_forward(&self, from: usize, re: &Regex) -> Option<usize> {
        (from..self.view.len()).find(|&i| {
            self.view_line(i)
                .map(|l| re.is_match(&l.raw))
                .unwrap_or(false)
        })
    }

    /// Find the previous view index at or before `from` whose line matches.
    pub fn search_backward(&self, from: usize, re: &Regex) -> Option<usize> {
        (0..=from.min(self.view.len().saturating_sub(1)))
            .rev()
            .find(|&i| {
                self.view_line(i)
                    .map(|l| re.is_match(&l.raw))
                    .unwrap_or(false)
            })
    }

    /// Render the whole filtered view as plain text (used by the export command).
    pub fn to_plain_text(&self) -> String {
        let mut out = String::with_capacity(self.view.len() * 96);
        for i in 0..self.view.len() {
            if let Some(line) = self.view_line(i) {
                out.push_str(&line.raw);
                out.push('\n');
            }
        }
        out
    }
}

/// Compiles the built-in and user-supplied highlight rules once, then applies
/// them to individual lines at draw time.
pub struct Highlighter {
    rules: Vec<(Regex, Style)>,
    theme: Theme,
}

impl Highlighter {
    pub fn new(theme: &Theme, user_rules: &[crate::config::HighlightRule]) -> Self {
        let mut rules: Vec<(Regex, Style)> = Vec::new();

        let mut add = |pattern: &str, style: Style| {
            if let Ok(re) = Regex::new(pattern) {
                rules.push((re, style));
            }
        };

        // Timestamps.
        add(
            r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?",
            Style::default().fg(Theme::color(&theme.trace)),
        );
        // IPv4 (+ optional port) and URLs.
        add(
            r"\b\d{1,3}(\.\d{1,3}){3}(:\d{1,5})?\b",
            Style::default().fg(Theme::color(&theme.accent)),
        );
        add(
            r#"\bhttps?://[^\s"']+"#,
            Style::default().fg(Theme::color(&theme.debug)),
        );
        // HTTP status codes.
        add(
            r"\b[45]\d{2}\b",
            Style::default().fg(Theme::color(&theme.error)),
        );
        add(
            r"\b2\d{2}\b",
            Style::default().fg(Theme::color(&theme.info)),
        );
        // Severity words, so they pop even inside INFO-coloured lines.
        add(
            r"(?i)\b(fatal|panic|error|exception|failed|failure)\b",
            Style::default()
                .fg(Theme::color(&theme.error))
                .add_modifier(Modifier::BOLD),
        );
        add(
            r"(?i)\b(warn|warning|retry|retrying|timeout|deadline)\b",
            Style::default().fg(Theme::color(&theme.warn)),
        );
        // Quoted strings and JSON keys.
        add(
            r#""[^"]{1,120}"\s*:"#,
            Style::default().fg(Theme::color(&theme.accent)),
        );
        // Kubernetes-ish identifiers: UUIDs.
        add(
            r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
            Style::default().fg(Theme::color(&theme.debug)),
        );

        for rule in user_rules {
            let mut style = Style::default().fg(Theme::color(&rule.fg));
            if rule.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            add(&rule.pattern, style);
        }

        Self {
            rules,
            theme: theme.clone(),
        }
    }

    /// Build a styled [`Line`] for a log line, overlaying the search hits.
    pub fn render<'a>(
        &self,
        line: &'a LogLine,
        search: Option<&Regex>,
        strip_timestamp: bool,
    ) -> Line<'a> {
        let text = if strip_timestamp {
            line.body()
        } else {
            &*line.raw
        };
        let base = line.level.style(&self.theme);

        // Collect non-overlapping spans, earlier rules win.
        let mut marks: Vec<(usize, usize, Style)> = Vec::new();
        for (re, style) in &self.rules {
            for m in re.find_iter(text).take(64) {
                marks.push((m.start(), m.end(), *style));
            }
        }
        if let Some(re) = search {
            let hit = self.theme.match_style();
            for m in re.find_iter(text).take(64) {
                marks.push((m.start(), m.end(), hit));
            }
        }

        if marks.is_empty() {
            return Line::from(Span::styled(text, base));
        }

        // Search hits must win: they were pushed last, so sort keeps them last
        // for equal starts and the dedup below prefers the later entry.
        marks.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

        let mut spans: Vec<Span<'a>> = Vec::with_capacity(marks.len() * 2 + 1);
        let mut cursor = 0usize;
        for (start, end, style) in marks {
            if start < cursor || end > text.len() {
                continue;
            }
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                continue;
            }
            if start > cursor {
                spans.push(Span::styled(&text[cursor..start], base));
            }
            spans.push(Span::styled(&text[start..end], base.patch(style)));
            cursor = end;
        }
        if cursor < text.len() {
            spans.push(Span::styled(&text[cursor..], base));
        }
        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> LogLine {
        LogLine::new(s.to_string(), Arc::from("test"))
    }

    #[test]
    fn classifies_common_formats() {
        assert_eq!(classify("ERROR something blew up"), Level::Error);
        assert_eq!(classify("level=warn msg=\"slow\""), Level::Warn);
        assert_eq!(classify("2024-01-01 DEBUG cache hit"), Level::Debug);
        assert_eq!(classify("plain message"), Level::Info);
        assert_eq!(classify("FATAL: goodbye"), Level::Fatal);
    }

    #[test]
    fn does_not_match_substrings() {
        // "err" inside "cherry" must not promote the line to ERROR.
        assert_eq!(classify("cherry picking commits"), Level::Info);
        assert_eq!(classify("terrace warmer"), Level::Info);
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut buf = LogBuffer::new(64);
        for i in 0..200 {
            buf.push(line(&format!("line {i}")));
        }
        assert_eq!(buf.len(), 64);
        assert_eq!(buf.received, 200);
        assert_eq!(buf.dropped, 136);
        assert_eq!(buf.view_len(), 64);
        assert_eq!(&*buf.view_line(0).unwrap().raw, "line 136");
    }

    #[test]
    fn filter_rebuild_and_incremental_agree() {
        let mut buf = LogBuffer::new(1000);
        for i in 0..100 {
            let text = if i % 5 == 0 {
                format!("ERROR boom {i}")
            } else {
                format!("info tick {i}")
            };
            buf.push(line(&text));
        }
        let mut f = Filter::default();
        f.errors_only = true;
        buf.set_filter(f);
        let after_rebuild = buf.view_len();
        buf.push(line("ERROR one more"));
        buf.push(line("just info"));
        assert_eq!(buf.view_len(), after_rebuild + 1);
    }

    #[test]
    fn search_finds_matches_in_view() {
        let mut buf = LogBuffer::new(100);
        buf.push(line("alpha"));
        buf.push(line("beta"));
        buf.push(line("gamma"));
        let re = Regex::new("beta").unwrap();
        assert_eq!(buf.search_forward(0, &re), Some(1));
        assert_eq!(buf.search_backward(2, &re), Some(1));
        assert_eq!(buf.search_forward(2, &re), None);
    }

    #[test]
    fn strips_api_server_timestamps() {
        let l = line("2024-05-01T10:11:12.123456789Z hello world");
        assert_eq!(l.body(), "hello world");
        assert!(l.timestamp().unwrap().starts_with("2024-05-01T"));
        let plain = line("no timestamp here");
        assert_eq!(plain.body(), "no timestamp here");
        assert!(plain.timestamp().is_none());
    }
}
