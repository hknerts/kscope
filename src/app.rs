//! Application state and the update half of the loop.
//!
//! Rendering lives in [`crate::ui`]; this module owns *what* is true and
//! [`crate::ui`] owns *how it looks*. State changes set `dirty`, and the main
//! loop redraws at most `general.max_fps` times per second.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use regex::{Regex, RegexBuilder};
use tokio::sync::mpsc::Sender;

use crate::config::Config;
use crate::k8s::discovery::{Scope, Update};
use crate::k8s::logs::{LogEvent, StreamSpec};
use crate::k8s::metrics::MetricsEvent;
use crate::k8s::{Inventory, PodInfo};
use crate::logs::{Highlighter, Level, LogBuffer, LogLine};
use crate::metrics::MetricsStore;

/// Which of the two tools is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Logs,
    Metrics,
}

/// Which pane has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    Content,
}

/// Modal text entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    Filter,
    Namespace,
    PodFilter,
    Help,
}

impl InputMode {
    pub fn prompt(self) -> &'static str {
        match self {
            InputMode::Search => "/",
            InputMode::Filter => "filter (prefix ! to exclude) > ",
            InputMode::Namespace => "namespace > ",
            InputMode::PodFilter => "pods matching > ",
            _ => "",
        }
    }
}

/// A row in the resource sidebar.
#[derive(Debug, Clone)]
pub enum SidebarItem {
    /// Header for pods sharing an owning workload (Deployment, StatefulSet,
    /// DaemonSet, Job, ...). Not itself a pod — `Enter` collapses/expands it.
    Group {
        key: String,
        kind: String,
        name: String,
    },
    Pod { key: String, pod: usize },
    Container { key: String, pod: usize, name: String },
}

impl SidebarItem {
    /// `None` for group headers, which have no backing pod.
    pub fn pod_index(&self) -> Option<usize> {
        match self {
            SidebarItem::Pod { pod, .. } | SidebarItem::Container { pod, .. } => Some(*pod),
            SidebarItem::Group { .. } => None,
        }
    }
    pub fn key(&self) -> &str {
        match self {
            SidebarItem::Group { key, .. }
            | SidebarItem::Pod { key, .. }
            | SidebarItem::Container { key, .. } => key,
        }
    }
}

/// Severity of a status-bar message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub text: String,
    pub kind: StatusKind,
    pub at: Instant,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            text: "welcome to kscope — press ? for help".into(),
            kind: StatusKind::Info,
            at: Instant::now(),
        }
    }
}

/// Current search state.
#[derive(Debug, Default)]
pub struct Search {
    pub query: String,
    pub regex: Option<Regex>,
    pub current: Option<usize>,
    pub total: usize,
}

/// Sort order for the metric tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Cpu,
    Memory,
}

impl SortBy {
    pub fn next(self) -> Self {
        match self {
            SortBy::Name => SortBy::Cpu,
            SortBy::Cpu => SortBy::Memory,
            SortBy::Memory => SortBy::Name,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Cpu => "cpu",
            SortBy::Memory => "mem",
        }
    }
}

/// Which metric table the content pane focuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricPane {
    Nodes,
    Pods,
    Volumes,
}

pub struct App {
    pub config: Config,
    pub client: kube::Client,
    pub scope: Scope,

    pub view: View,
    pub pane: Pane,
    pub mode: InputMode,
    pub input: String,

    pub inventory: Inventory,
    pub sidebar: Vec<SidebarItem>,
    pub sidebar_selected: usize,
    pub sidebar_offset: usize,
    pub expanded: HashSet<String>,
    pub collapsed_groups: HashSet<String>,
    pub pod_filter: Option<String>,

    pub buffer: LogBuffer,
    pub highlighter: Highlighter,
    pub follow: bool,
    pub wrap: bool,
    pub timestamps: bool,
    pub previous: bool,
    /// Index of the top visible line in the filtered view.
    pub scroll: usize,
    pub h_scroll: usize,
    pub viewport_height: usize,
    pub search: Search,

    pub metrics: MetricsStore,
    pub metric_pane: MetricPane,
    pub node_selected: usize,
    pub pod_metric_selected: usize,
    pub volume_selected: usize,
    pub sort_by: SortBy,

    pub attached: Vec<Arc<str>>,
    streams: HashMap<Arc<str>, tokio::task::JoinHandle<()>>,
    log_tx: Sender<LogEvent>,

    pub status: Status,
    pub dirty: bool,
    pub should_quit: bool,
    pub started: Instant,
}

impl App {
    pub fn new(config: Config, client: kube::Client, scope: Scope, log_tx: Sender<LogEvent>) -> Self {
        let highlighter = Highlighter::new(&config.theme, &config.highlight);
        let buffer = LogBuffer::new(config.logs.buffer_lines);
        let metrics = MetricsStore::new(config.metrics.history);
        Self {
            follow: config.logs.follow,
            wrap: config.logs.wrap,
            timestamps: config.logs.timestamps,
            previous: false,
            config,
            client,
            scope,
            view: View::Logs,
            pane: Pane::Sidebar,
            mode: InputMode::Normal,
            input: String::new(),
            inventory: Inventory::default(),
            sidebar: Vec::new(),
            sidebar_selected: 0,
            sidebar_offset: 0,
            expanded: HashSet::new(),
            collapsed_groups: HashSet::new(),
            pod_filter: None,
            buffer,
            highlighter,
            scroll: 0,
            h_scroll: 0,
            viewport_height: 20,
            search: Search::default(),
            metrics,
            metric_pane: MetricPane::Pods,
            node_selected: 0,
            pod_metric_selected: 0,
            volume_selected: 0,
            sort_by: SortBy::Cpu,
            attached: Vec::new(),
            streams: HashMap::new(),
            log_tx,
            status: Status::default(),
            dirty: true,
            should_quit: false,
            started: Instant::now(),
        }
    }

    // ---------------------------------------------------------------- status

    pub fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        self.status = Status {
            text: text.into(),
            kind,
            at: Instant::now(),
        };
        self.dirty = true;
    }

    // ------------------------------------------------------------- inventory

    pub fn on_update(&mut self, update: Update) {
        match update {
            Update::Inventory(inv) => {
                self.inventory = *inv;
                self.rebuild_sidebar();
                self.reconcile_metrics_metadata();
                self.dirty = true;
            }
            Update::Warning(msg) => self.set_status(msg, StatusKind::Warn),
        }
    }

    /// Copy requests/limits and node placement from the inventory onto the
    /// metric store so the metrics view can show usage-vs-limit ratios.
    fn reconcile_metrics_metadata(&mut self) {
        for pod in &self.inventory.pods {
            if let Some(entry) = self.metrics.pods.get_mut(&pod.key()) {
                entry.node = pod.node.clone();
                entry.phase = pod.phase.clone();
                entry.cpu_request = pod.containers.iter().map(|c| c.cpu_request).sum();
                entry.cpu_limit = pod.containers.iter().map(|c| c.cpu_limit).sum();
                entry.mem_request = pod.containers.iter().map(|c| c.mem_request).sum();
                entry.mem_limit = pod.containers.iter().map(|c| c.mem_limit).sum();
                for c in &pod.containers {
                    if let Some(cm) = entry.containers.iter_mut().find(|x| x.name == c.name) {
                        cm.cpu_request = c.cpu_request;
                        cm.cpu_limit = c.cpu_limit;
                        cm.mem_request = c.mem_request;
                        cm.mem_limit = c.mem_limit;
                        cm.restarts = c.restarts;
                        cm.ready = c.ready;
                        cm.state = c.state.clone();
                    }
                }
            }
        }
        let mut pods_per_node: HashMap<&str, usize> = HashMap::new();
        for pod in &self.inventory.pods {
            *pods_per_node.entry(pod.node.as_str()).or_default() += 1;
        }
        for node in &self.inventory.nodes {
            if let Some(entry) = self.metrics.nodes.get_mut(&node.name) {
                entry.cpu_capacity = node.cpu_capacity;
                entry.mem_capacity = node.mem_capacity;
                entry.cpu_allocatable = node.cpu_allocatable;
                entry.mem_allocatable = node.mem_allocatable;
                entry.ready = node.ready;
                entry.version = node.version.clone();
                entry.pods = pods_per_node.get(node.name.as_str()).copied().unwrap_or(0);
            }
        }
    }

    pub fn rebuild_sidebar(&mut self) {
        let previous = self
            .sidebar
            .get(self.sidebar_selected)
            .map(|i| i.key().to_string());

        let needle = self.pod_filter.as_deref().map(str::to_ascii_lowercase);

        // Bucket pods under their owning workload, preserving first-seen
        // order so the sidebar stays close to the existing namespace/name
        // sort. Pods without a recognised owner render exactly as before —
        // no header, just the pod row.
        enum Entry {
            Standalone(usize),
            Group(String),
        }
        struct Group {
            kind: String,
            name: String,
            pods: Vec<usize>,
        }

        let mut entries: Vec<Entry> = Vec::with_capacity(self.inventory.pods.len());
        let mut groups: HashMap<String, Group> = HashMap::new();
        for (idx, pod) in self.inventory.pods.iter().enumerate() {
            if let Some(n) = &needle {
                if !pod.name.to_ascii_lowercase().contains(n)
                    && !pod.namespace.to_ascii_lowercase().contains(n)
                {
                    continue;
                }
            }
            if pod.owner_kind.is_empty() || pod.owner_name.is_empty() {
                entries.push(Entry::Standalone(idx));
                continue;
            }
            let gkey = format!("{}/{}/{}", pod.namespace, pod.owner_kind, pod.owner_name);
            match groups.get_mut(&gkey) {
                Some(g) => g.pods.push(idx),
                None => {
                    groups.insert(
                        gkey.clone(),
                        Group {
                            kind: pod.owner_kind.clone(),
                            name: pod.owner_name.clone(),
                            pods: vec![idx],
                        },
                    );
                    entries.push(Entry::Group(gkey));
                }
            }
        }

        let mut items = Vec::with_capacity(self.inventory.pods.len() * 2);
        let push_pod = |items: &mut Vec<SidebarItem>, idx: usize| {
            let pod = &self.inventory.pods[idx];
            let key = pod.key();
            let expanded = self.expanded.contains(&key);
            items.push(SidebarItem::Pod {
                key: key.clone(),
                pod: idx,
            });
            if expanded {
                for c in &pod.containers {
                    items.push(SidebarItem::Container {
                        key: format!("{key}:{}", c.name),
                        pod: idx,
                        name: c.name.clone(),
                    });
                }
            }
        };
        for entry in entries {
            match entry {
                Entry::Standalone(idx) => push_pod(&mut items, idx),
                Entry::Group(gkey) => {
                    let group = &groups[&gkey];
                    items.push(SidebarItem::Group {
                        key: gkey.clone(),
                        kind: group.kind.clone(),
                        name: group.name.clone(),
                    });
                    if !self.collapsed_groups.contains(&gkey) {
                        for &idx in &group.pods {
                            push_pod(&mut items, idx);
                        }
                    }
                }
            }
        }
        self.sidebar = items;

        if let Some(prev) = previous {
            if let Some(pos) = self.sidebar.iter().position(|i| i.key() == prev) {
                self.sidebar_selected = pos;
            }
        }
        self.sidebar_selected = self
            .sidebar_selected
            .min(self.sidebar.len().saturating_sub(1));
    }

    pub fn selected_pod(&self) -> Option<&PodInfo> {
        let item = self.sidebar.get(self.sidebar_selected)?;
        self.inventory.pods.get(item.pod_index()?)
    }

    // ------------------------------------------------------------------ logs

    pub fn on_log(&mut self, event: LogEvent) {
        match event {
            LogEvent::Batch { source, lines } => {
                for line in lines {
                    self.buffer.push(LogLine::new(line, source.clone()));
                }
                if self.follow {
                    self.scroll_to_bottom();
                }
                self.refresh_search_count();
                self.dirty = true;
            }
            LogEvent::Attached(source) => {
                self.set_status(format!("streaming {source}"), StatusKind::Info)
            }
            LogEvent::Ended(source) => {
                self.set_status(format!("{source} stream ended — retrying"), StatusKind::Warn)
            }
            LogEvent::Failed { source, error } => {
                self.set_status(format!("{source}: {error}"), StatusKind::Error)
            }
        }
    }

    pub fn attach_selected(&mut self, all_containers: bool) {
        let Some(item) = self.sidebar.get(self.sidebar_selected).cloned() else {
            self.set_status("nothing selected", StatusKind::Warn);
            return;
        };
        let Some(pod_idx) = item.pod_index() else {
            self.set_status("select a pod, not a workload group", StatusKind::Warn);
            return;
        };
        let Some(pod) = self.inventory.pods.get(pod_idx).cloned() else {
            return;
        };

        let containers: Vec<Option<String>> = match (&item, all_containers) {
            (SidebarItem::Container { name, .. }, false) => vec![Some(name.clone())],
            _ => pod
                .containers
                .iter()
                .filter(|c| !c.init)
                .map(|c| Some(c.name.clone()))
                .collect(),
        };

        // Picking a single container (`Enter` on a container row) switches
        // the view to just that container's logs. Bulk-attaching (`a`) stays
        // additive — that is the merged multi-container view.
        if matches!(item, SidebarItem::Container { .. }) && !all_containers {
            self.detach_all();
            self.buffer.clear();
            self.scroll = 0;
        }

        for container in containers {
            let spec = StreamSpec {
                namespace: pod.namespace.clone(),
                pod: pod.name.clone(),
                container,
                tail: self.tail_lines(),
                since_seconds: self.config.logs.since_seconds,
                timestamps: self.timestamps,
                previous: self.previous,
            };
            let source = spec.source();
            if self.streams.contains_key(&source) {
                continue;
            }
            let handle = crate::k8s::logs::spawn(self.client.clone(), spec, self.log_tx.clone());
            self.streams.insert(source.clone(), handle);
            self.attached.push(source);
        }
        self.view = View::Logs;
        self.pane = Pane::Content;
        self.dirty = true;
    }

    /// `None` asks for the container's entire retained history.
    fn tail_lines(&self) -> Option<i64> {
        if self.config.logs.tail_lines <= 0 {
            None
        } else {
            Some(self.config.logs.tail_lines)
        }
    }

    pub fn detach_all(&mut self) {
        for (_, handle) in self.streams.drain() {
            handle.abort();
        }
        self.attached.clear();
        self.set_status("detached from all streams", StatusKind::Info);
    }

    /// Re-attach every current stream, e.g. after toggling timestamps.
    fn restream(&mut self) {
        let sources: Vec<Arc<str>> = self.attached.clone();
        self.detach_all();
        for source in sources {
            let (ns_pod, container) = match source.split_once(':') {
                Some((a, b)) => (a.to_string(), Some(b.to_string())),
                None => (source.to_string(), None),
            };
            let Some((namespace, pod)) = ns_pod.split_once('/') else {
                continue;
            };
            let spec = StreamSpec {
                namespace: namespace.to_string(),
                pod: pod.to_string(),
                container,
                tail: self.tail_lines(),
                since_seconds: self.config.logs.since_seconds,
                timestamps: self.timestamps,
                previous: self.previous,
            };
            let src = spec.source();
            let handle = crate::k8s::logs::spawn(self.client.clone(), spec, self.log_tx.clone());
            self.streams.insert(src.clone(), handle);
            self.attached.push(src);
        }
    }

    // -------------------------------------------------------------- metrics

    pub fn on_metrics(&mut self, event: MetricsEvent) {
        match event {
            MetricsEvent::Snapshot(snapshot) => {
                self.metrics.error = None;
                self.metrics.last_update = Some(Instant::now());
                for node in &snapshot.nodes {
                    self.metrics
                        .record_node(&node.name, node.cpu_milli, node.mem_bytes);
                }
                let mut keys = HashSet::with_capacity(snapshot.pods.len());
                for pod in &snapshot.pods {
                    let containers: Vec<(String, f64, f64)> = pod
                        .containers
                        .iter()
                        .map(|c| (c.name.clone(), c.cpu_milli, c.mem_bytes))
                        .collect();
                    self.metrics
                        .record_pod(&pod.namespace, &pod.name, &containers);
                    keys.insert(format!("{}/{}", pod.namespace, pod.name));
                }
                self.metrics.retain_pods(&keys);
                self.reconcile_metrics_metadata();
                self.dirty = true;
            }
            MetricsEvent::Unavailable(err) => {
                self.metrics.error = Some(err.clone());
                self.set_status(err, StatusKind::Error);
            }
        }
    }

    /// Pod metric rows in the currently selected sort order.
    pub fn sorted_pod_metrics(&self) -> Vec<&crate::metrics::PodMetrics> {
        let mut rows: Vec<_> = self.metrics.pods.values().collect();
        match self.sort_by {
            SortBy::Name => rows.sort_by(|a, b| a.key().cmp(&b.key())),
            SortBy::Cpu => rows.sort_by(|a, b| {
                b.usage
                    .cpu
                    .last()
                    .partial_cmp(&a.usage.cpu.last())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortBy::Memory => rows.sort_by(|a, b| {
                b.usage
                    .mem
                    .last()
                    .partial_cmp(&a.usage.mem.last())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
        }
        rows
    }

    pub fn sorted_node_metrics(&self) -> Vec<&crate::metrics::NodeMetrics> {
        let mut rows: Vec<_> = self.metrics.nodes.values().collect();
        match self.sort_by {
            SortBy::Name => rows.sort_by(|a, b| a.name.cmp(&b.name)),
            SortBy::Cpu => rows.sort_by(|a, b| {
                b.cpu_pct()
                    .partial_cmp(&a.cpu_pct())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortBy::Memory => rows.sort_by(|a, b| {
                b.mem_pct()
                    .partial_cmp(&a.mem_pct())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
        }
        rows
    }

    // -------------------------------------------------------------- scrolling

    pub fn max_scroll(&self) -> usize {
        self.buffer
            .view_len()
            .saturating_sub(self.viewport_height.max(1))
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = self.max_scroll();
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.max_scroll();
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, max as isize) as usize;
        // Any upward movement leaves follow mode; reaching the end re-enters it.
        if delta < 0 {
            self.follow = false;
        } else if self.scroll >= max {
            self.follow = true;
        }
        self.dirty = true;
    }

    // ---------------------------------------------------------------- search

    fn compile(&self, pattern: &str) -> Option<Regex> {
        if pattern.is_empty() {
            return None;
        }
        let smart = self.config.logs.smart_case;
        let insensitive = smart && !pattern.chars().any(|c| c.is_uppercase());
        RegexBuilder::new(pattern)
            .case_insensitive(insensitive)
            .build()
            .ok()
            .or_else(|| {
                RegexBuilder::new(&regex::escape(pattern))
                    .case_insensitive(insensitive)
                    .build()
                    .ok()
            })
    }

    fn refresh_search_count(&mut self) {
        let Some(re) = &self.search.regex else {
            self.search.total = 0;
            return;
        };
        let mut total = 0usize;
        let mut idx = 0usize;
        while let Some(hit) = self.buffer.search_forward(idx, re) {
            total += 1;
            idx = hit + 1;
            if total >= 100_000 {
                break;
            }
        }
        self.search.total = total;
    }

    pub fn search_next(&mut self, backwards: bool) {
        let Some(re) = self.search.regex.clone() else {
            return;
        };
        let from = self.search.current.unwrap_or(self.scroll);
        let hit = if backwards {
            self.buffer
                .search_backward(from.saturating_sub(1), &re)
                .or_else(|| {
                    self.buffer
                        .search_backward(self.buffer.view_len().saturating_sub(1), &re)
                })
        } else {
            self.buffer
                .search_forward(from + 1, &re)
                .or_else(|| self.buffer.search_forward(0, &re))
        };
        match hit {
            Some(index) => {
                self.search.current = Some(index);
                self.follow = false;
                let half = self.viewport_height / 2;
                self.scroll = index.saturating_sub(half).min(self.max_scroll());
                self.dirty = true;
            }
            None => self.set_status("no matches", StatusKind::Warn),
        }
    }

    // ------------------------------------------------------------- filtering

    fn apply_filter_input(&mut self, raw: &str) {
        let mut filter = self.buffer.filter.clone();
        let raw = raw.trim();
        if raw.is_empty() {
            filter.include = None;
            filter.exclude = None;
        } else if let Some(rest) = raw.strip_prefix('!') {
            filter.exclude = self.compile(rest);
        } else {
            filter.include = self.compile(raw);
        }
        let text = filter.describe();
        self.buffer.set_filter(filter);
        self.scroll_to_bottom();
        self.refresh_search_count();
        self.set_status(format!("filter: {text}"), StatusKind::Info);
    }

    // ------------------------------------------------------------------ keys

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Resize(_, _) => self.dirty = true,
            Event::Mouse(m) if self.config.general.mouse => match m.kind {
                MouseEventKind::ScrollUp => self.scroll_by(-3),
                MouseEventKind::ScrollDown => self.scroll_by(3),
                _ => {}
            },
            _ => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Text-entry modes swallow most keys.
        if matches!(
            self.mode,
            InputMode::Search | InputMode::Filter | InputMode::Namespace | InputMode::PodFilter
        ) {
            self.on_prompt_key(key);
            return;
        }
        if self.mode == InputMode::Help {
            self.mode = InputMode::Normal;
            self.dirty = true;
            return;
        }

        // Control chords are handled first so they never collide with the
        // plain-key bindings below.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => self.should_quit = true,
                KeyCode::Char('d') => self.move_cursor(self.viewport_height as isize / 2),
                KeyCode::Char('u') => self.move_cursor(-(self.viewport_height as isize / 2)),
                KeyCode::Char('f') => self.move_cursor(self.viewport_height as isize),
                KeyCode::Char('b') => self.move_cursor(-(self.viewport_height as isize)),
                KeyCode::Char('n') => self.enter_prompt(InputMode::Namespace),
                KeyCode::Char('p') => self.enter_prompt(InputMode::PodFilter),
                KeyCode::Char('l') => self.dirty = true,
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => {
                self.mode = InputMode::Help;
                self.dirty = true;
            }

            // views and panes
            KeyCode::Char('1') => self.set_view(View::Logs),
            KeyCode::Char('2') => self.set_view(View::Metrics),
            KeyCode::Tab => {
                self.pane = match self.pane {
                    Pane::Sidebar => Pane::Content,
                    Pane::Content => Pane::Sidebar,
                };
                self.dirty = true;
            }
            KeyCode::BackTab => {
                self.set_view(if self.view == View::Logs {
                    View::Metrics
                } else {
                    View::Logs
                });
            }

            // navigation
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::PageDown => self.move_cursor(self.viewport_height as isize),
            KeyCode::PageUp => self.move_cursor(-(self.viewport_height as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.goto_start(),
            KeyCode::Char('G') | KeyCode::End => self.goto_end(),
            KeyCode::Left | KeyCode::Char('h') => {
                self.h_scroll = self.h_scroll.saturating_sub(8);
                self.dirty = true;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.h_scroll = (self.h_scroll + 8).min(4096);
                self.dirty = true;
            }

            // selection
            KeyCode::Enter => self.activate_selection(),
            KeyCode::Char('a') => self.attach_selected(true),
            KeyCode::Char('x') => self.detach_all(),

            // log controls
            KeyCode::Char('/') => self.enter_prompt(InputMode::Search),
            KeyCode::Char('\\') => self.enter_prompt(InputMode::Filter),
            KeyCode::Char('n') => self.search_next(false),
            KeyCode::Char('N') => self.search_next(true),
            KeyCode::Char('F') => {
                self.follow = !self.follow;
                if self.follow {
                    self.scroll_to_bottom();
                }
                let msg = if self.follow { "follow on" } else { "follow off" };
                self.set_status(msg, StatusKind::Info);
            }
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                self.h_scroll = 0;
                let msg = if self.wrap { "wrap on" } else { "wrap off" };
                self.set_status(msg, StatusKind::Info);
            }
            KeyCode::Char('L') => {
                let mut filter = self.buffer.filter.clone();
                filter.min_level = Level::next_threshold(filter.min_level);
                let label = filter.min_level.label();
                self.buffer.set_filter(filter);
                self.scroll_to_bottom();
                self.set_status(format!("level >= {label}"), StatusKind::Info);
            }
            KeyCode::Char('e') => {
                let mut filter = self.buffer.filter.clone();
                filter.errors_only = !filter.errors_only;
                let on = filter.errors_only;
                self.buffer.set_filter(filter);
                self.scroll_to_bottom();
                self.set_status(
                    if on {
                        "showing errors only"
                    } else {
                        "showing all levels"
                    },
                    StatusKind::Info,
                );
            }
            KeyCode::Char('t') => {
                self.timestamps = !self.timestamps;
                self.restream();
                self.set_status(
                    format!("timestamps {}", if self.timestamps { "on" } else { "off" }),
                    StatusKind::Info,
                );
            }
            KeyCode::Char('p') => {
                self.previous = !self.previous;
                self.restream();
                self.set_status(
                    format!(
                        "previous container logs {}",
                        if self.previous { "on" } else { "off" }
                    ),
                    StatusKind::Info,
                );
            }
            KeyCode::Char('c') => {
                self.buffer.clear();
                self.scroll = 0;
                self.set_status("buffer cleared", StatusKind::Info);
            }
            KeyCode::Char('s') => self.save_buffer(),

            // scoping
            KeyCode::Char('S') => {
                self.sort_by = self.sort_by.next();
                let label = self.sort_by.label();
                self.set_status(format!("sorting by {label}"), StatusKind::Info);
            }
            KeyCode::Char('m') => {
                self.metric_pane = match self.metric_pane {
                    MetricPane::Nodes => MetricPane::Pods,
                    MetricPane::Pods => MetricPane::Volumes,
                    MetricPane::Volumes => MetricPane::Nodes,
                };
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn on_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.input.clear();
                self.dirty = true;
            }
            KeyCode::Enter => {
                let value = std::mem::take(&mut self.input);
                let mode = self.mode;
                self.mode = InputMode::Normal;
                match mode {
                    InputMode::Search => {
                        self.search.query = value.clone();
                        self.search.regex = self.compile(&value);
                        self.refresh_search_count();
                        self.search.current = None;
                        self.search_next(false);
                    }
                    InputMode::Filter => self.apply_filter_input(&value),
                    InputMode::Namespace => self.set_namespace(&value),
                    InputMode::PodFilter => {
                        self.pod_filter = if value.trim().is_empty() {
                            None
                        } else {
                            Some(value.trim().to_string())
                        };
                        self.rebuild_sidebar();
                    }
                    _ => {}
                }
                self.dirty = true;
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.live_preview();
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.live_preview();
            }
            _ => {}
        }
    }

    /// Incremental feedback while typing a search or pod filter.
    fn live_preview(&mut self) {
        match self.mode {
            InputMode::Search => {
                self.search.query = self.input.clone();
                self.search.regex = self.compile(&self.input);
                self.refresh_search_count();
            }
            InputMode::PodFilter => {
                self.pod_filter = if self.input.is_empty() {
                    None
                } else {
                    Some(self.input.clone())
                };
                self.rebuild_sidebar();
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn enter_prompt(&mut self, mode: InputMode) {
        self.mode = mode;
        self.input.clear();
        self.dirty = true;
    }

    fn set_view(&mut self, view: View) {
        self.view = view;
        self.dirty = true;
    }

    fn set_namespace(&mut self, value: &str) {
        let value = value.trim();
        self.scope = if value.is_empty() || value == "all" || value == "*" {
            Scope::AllNamespaces
        } else {
            Scope::Namespace(value.to_string())
        };
        self.set_status(
            format!("namespace scope: {} (restart pollers)", self.scope.label()),
            StatusKind::Info,
        );
    }

    fn move_cursor(&mut self, delta: isize) {
        match (self.pane, self.view) {
            (Pane::Sidebar, _) => {
                let len = self.sidebar.len();
                if len == 0 {
                    return;
                }
                let next = (self.sidebar_selected as isize + delta).clamp(0, len as isize - 1);
                self.sidebar_selected = next as usize;
                self.dirty = true;
            }
            (Pane::Content, View::Logs) => self.scroll_by(delta),
            (Pane::Content, View::Metrics) => {
                match self.metric_pane {
                    MetricPane::Nodes => {
                        let len = self.metrics.nodes.len();
                        if len > 0 {
                            self.node_selected = (self.node_selected as isize + delta)
                                .clamp(0, len as isize - 1)
                                as usize;
                        }
                    }
                    MetricPane::Pods => {
                        let len = self.metrics.pods.len();
                        if len > 0 {
                            self.pod_metric_selected = (self.pod_metric_selected as isize + delta)
                                .clamp(0, len as isize - 1)
                                as usize;
                        }
                    }
                    MetricPane::Volumes => {
                        let len = self.inventory.volumes.len();
                        if len > 0 {
                            self.volume_selected = (self.volume_selected as isize + delta)
                                .clamp(0, len as isize - 1)
                                as usize;
                        }
                    }
                }
                self.dirty = true;
            }
        }
    }

    fn goto_start(&mut self) {
        match self.pane {
            Pane::Sidebar => self.sidebar_selected = 0,
            Pane::Content => {
                self.follow = false;
                self.scroll = 0;
            }
        }
        self.dirty = true;
    }

    fn goto_end(&mut self) {
        match self.pane {
            Pane::Sidebar => self.sidebar_selected = self.sidebar.len().saturating_sub(1),
            Pane::Content => {
                self.follow = true;
                self.scroll_to_bottom();
            }
        }
        self.dirty = true;
    }

    fn activate_selection(&mut self) {
        if self.pane != Pane::Sidebar {
            return;
        }
        let Some(item) = self.sidebar.get(self.sidebar_selected).cloned() else {
            return;
        };
        match item {
            SidebarItem::Group { key, .. } => {
                if !self.collapsed_groups.remove(&key) {
                    self.collapsed_groups.insert(key);
                }
                self.rebuild_sidebar();
                self.dirty = true;
            }
            SidebarItem::Pod { key, .. } => {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
                self.rebuild_sidebar();
                self.dirty = true;
            }
            SidebarItem::Container { .. } => self.attach_selected(false),
        }
    }

    fn save_buffer(&mut self) {
        let name = format!(
            "kscope-{}.log",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        match std::fs::write(&name, self.buffer.to_plain_text()) {
            Ok(()) => self.set_status(
                format!("wrote {} lines to {name}", self.buffer.view_len()),
                StatusKind::Info,
            ),
            Err(err) => self.set_status(format!("save failed: {err}"), StatusKind::Error),
        }
    }

    /// Elapsed since the last metrics poll, for the status bar.
    pub fn metrics_age(&self) -> Option<Duration> {
        self.metrics.last_update.map(|t| t.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_cycles() {
        assert_eq!(SortBy::Name.next(), SortBy::Cpu);
        assert_eq!(SortBy::Cpu.next(), SortBy::Memory);
        assert_eq!(SortBy::Memory.next(), SortBy::Name);
    }

    #[test]
    fn level_threshold_cycles_through_all() {
        let mut level = Level::Trace;
        for _ in 0..6 {
            level = Level::next_threshold(level);
        }
        assert_eq!(level, Level::Trace);
    }
}
