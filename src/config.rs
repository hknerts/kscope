//! User configuration: loaded from `$XDG_CONFIG_HOME/kscope/config.toml`.
//!
//! Every field is optional; missing values fall back to the defaults defined
//! here, so a brand new user never has to write a config file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

/// Root configuration object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub logs: LogsConfig,
    pub metrics: MetricsConfig,
    pub theme: Theme,
    /// Extra user-defined regex highlight rules, applied after the built-ins.
    pub highlight: Vec<HighlightRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Redraw budget: the UI never renders faster than this many times/second.
    pub max_fps: u16,
    /// Namespace selected on start-up. `None` means "use kubeconfig default".
    pub namespace: Option<String>,
    /// Refresh interval for the pod/node inventory, in milliseconds.
    pub inventory_refresh_ms: u64,
    /// Refresh interval for cluster events, in milliseconds. Slower than the
    /// inventory by default: events are cheap to read but rarely worth
    /// re-listing more than a couple of times a minute.
    pub events_refresh_ms: u64,
    /// Mouse support (scroll wheel + click to select).
    pub mouse: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            max_fps: 30,
            namespace: None,
            inventory_refresh_ms: 5_000,
            events_refresh_ms: 10_000,
            mouse: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogsConfig {
    /// Maximum number of lines kept in memory. **`0` means unlimited**, which
    /// is the default: nothing is ever dropped, so you can always scroll back
    /// to the first line of the session. Set a positive value to turn the
    /// buffer into a ring that evicts the oldest lines instead.
    pub buffer_lines: usize,
    /// How many historical lines to request when attaching to a container.
    /// **`0` means "everything the API server still has"**, i.e. from the
    /// moment the container started. A positive value only narrows that.
    pub tail_lines: i64,
    /// Only fetch lines newer than this many seconds. `None` means no limit.
    pub since_seconds: Option<i64>,
    /// Ask the API server for RFC3339 timestamps on every line.
    pub timestamps: bool,
    /// Start in "follow" (auto-scroll) mode.
    pub follow: bool,
    /// Soft-wrap long lines instead of horizontal scrolling.
    pub wrap: bool,
    /// Searches are case-insensitive unless the query contains an upper-case
    /// character (smart case), when this is true.
    pub smart_case: bool,
}

impl Default for LogsConfig {
    fn default() -> Self {
        Self {
            buffer_lines: 0,
            tail_lines: 0,
            since_seconds: None,
            timestamps: false,
            follow: true,
            wrap: false,
            smart_case: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    /// Poll interval against `metrics.k8s.io`, in milliseconds.
    pub refresh_ms: u64,
    /// Number of samples retained per series (drives the sparkline width).
    pub history: usize,
    /// Warn / critical thresholds in percent of the request or capacity.
    pub warn_pct: f64,
    pub critical_pct: f64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            refresh_ms: 5_000,
            history: 240,
            warn_pct: 75.0,
            critical_pct: 90.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub fg: String,
    pub bg: String,
    pub accent: String,
    pub border: String,
    pub border_focus: String,
    pub error: String,
    pub warn: String,
    pub info: String,
    pub debug: String,
    pub trace: String,
    pub match_fg: String,
    pub match_bg: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: "gray".into(),
            bg: "reset".into(),
            accent: "cyan".into(),
            border: "darkgray".into(),
            border_focus: "cyan".into(),
            error: "red".into(),
            warn: "yellow".into(),
            info: "green".into(),
            debug: "blue".into(),
            trace: "darkgray".into(),
            match_fg: "black".into(),
            match_bg: "yellow".into(),
        }
    }
}

impl Theme {
    pub fn color(name: &str) -> Color {
        match name.trim().to_ascii_lowercase().as_str() {
            "reset" | "" => Color::Reset,
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "gray" | "grey" | "white" => Color::Gray,
            "darkgray" | "darkgrey" => Color::DarkGray,
            "lightred" => Color::LightRed,
            "lightgreen" => Color::LightGreen,
            "lightyellow" => Color::LightYellow,
            "lightblue" => Color::LightBlue,
            "lightmagenta" => Color::LightMagenta,
            "lightcyan" => Color::LightCyan,
            other => parse_hex(other).unwrap_or(Color::Reset),
        }
    }

    pub fn accent(&self) -> Color {
        Self::color(&self.accent)
    }
    pub fn border(&self) -> Color {
        Self::color(&self.border)
    }
    pub fn border_focus(&self) -> Color {
        Self::color(&self.border_focus)
    }
    pub fn error(&self) -> Color {
        Self::color(&self.error)
    }
    pub fn warn(&self) -> Color {
        Self::color(&self.warn)
    }
    pub fn match_style(&self) -> Style {
        Style::default()
            .fg(Self::color(&self.match_fg))
            .bg(Self::color(&self.match_bg))
            .add_modifier(Modifier::BOLD)
    }
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// A user-supplied regex highlight rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightRule {
    pub pattern: String,
    #[serde(default = "default_rule_fg")]
    pub fg: String,
    #[serde(default)]
    pub bold: bool,
}

fn default_rule_fg() -> String {
    "magenta".into()
}

impl Config {
    /// Default location of the config file.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("io", "kscope", "kscope")
            .map(|d| d.config_dir().join("config.toml"))
    }

    /// Load from an explicit path, or from the default location when `None`.
    /// A missing file is not an error — defaults are returned instead.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path.map(PathBuf::from).or_else(Self::default_path) {
            Some(p) => p,
            None => return Ok(Self::default()),
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing config file {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        // 0 == unlimited on both: full history, nothing evicted.
        assert_eq!(c.logs.buffer_lines, 0);
        assert_eq!(c.logs.tail_lines, 0);
        assert!(c.metrics.refresh_ms >= 1000);
        assert!(c.general.max_fps > 0);
    }

    #[test]
    fn parses_partial_config() {
        let c: Config = toml::from_str("[logs]\nbuffer_lines = 10\n").unwrap();
        assert_eq!(c.logs.buffer_lines, 10);
        // untouched section keeps its default
        assert_eq!(c.metrics.history, MetricsConfig::default().history);
    }

    #[test]
    fn parses_colors() {
        assert_eq!(Theme::color("red"), Color::Red);
        assert_eq!(Theme::color("#ff8800"), Color::Rgb(0xff, 0x88, 0x00));
        assert_eq!(Theme::color("nonsense"), Color::Reset);
    }
}
