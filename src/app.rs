//! Application state and the update half of the loop.
//!
//! Rendering lives in [`crate::ui`]; this module owns *what* is true and
//! [`crate::ui`] owns *how it looks*. State changes set `dirty`, and the main
//! loop redraws at most `general.max_fps` times per second.
//!
//! The shape of the UI is two panes: the **contexts** on the left, and on the
//! right a resource browser. `:` picks the resource *type* (with k9s-style
//! autocompletion over whatever the cluster's discovery API serves), the
//! browser lists that type's objects, and `Enter` opens one to inspect its
//! logs, metrics and events.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use regex::{Regex, RegexBuilder};
use tokio::sync::mpsc::Sender;

use crate::config::Config;
use crate::k8s::discovery::{Scope, Update};
use crate::k8s::events::{EventInfo, EventUpdate};
use crate::k8s::logs::{LogEvent, StreamSpec};
use crate::k8s::metrics::MetricsEvent;
use crate::k8s::resources::{ResourceRow, ResourceType};
use crate::k8s::{Inventory, PodInfo};
use crate::logs::{Highlighter, Level, LogBuffer, LogLine};
use crate::metrics::MetricsStore;
use crate::palette::{Candidate, Palette};

/// Which detail tab is on screen for the selected object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Logs,
    Metrics,
    Events,
}

/// Which pane has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Contexts,
    Resources,
}

/// What the right-hand pane is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightMode {
    /// Listing objects of the current resource type.
    Browse,
    /// Inspecting one object's logs / metrics / events.
    Detail,
}

/// Modal text entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    Filter,
    Namespace,
    /// Free-text filter over the browsed object list.
    RowFilter,
    /// The `:` resource-type palette.
    Command,
    Help,
}

impl InputMode {
    pub fn prompt(self) -> &'static str {
        match self {
            InputMode::Search => "/",
            InputMode::Filter => "filter (prefix ! to exclude) > ",
            InputMode::Namespace => "namespace > ",
            InputMode::RowFilter => "matching > ",
            InputMode::Command => ":",
            _ => "",
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
            text: "welcome to kscope — press : to pick a resource, ? for help".into(),
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

/// A kubeconfig context in the left-hand pane.
#[derive(Debug, Clone)]
pub struct ContextEntry {
    pub name: String,
    pub cluster: String,
}

pub struct App {
    pub config: Config,
    pub client: kube::Client,
    pub scope: Scope,
    /// kubeconfig context/user and apiserver version, for the header. Best
    /// effort — empty/"unknown" when running in-cluster or unreachable.
    pub context_name: String,
    pub user_name: String,
    pub k8s_version: String,

    // ------------------------------------------------------------- contexts
    pub contexts: Vec<ContextEntry>,
    pub context_selected: usize,
    /// Set when the user picks a different context; the main loop notices,
    /// rebuilds the client and restarts every poller.
    pub pending_context: Option<String>,
    /// Set when the namespace scope changes: the pollers are bound to a scope
    /// at spawn time, so they have to be restarted to follow it.
    pub pollers_stale: bool,

    // ------------------------------------------------------------ resources
    /// Every listable kind the cluster serves, from its discovery API.
    pub resource_types: Vec<ResourceType>,
    /// Index into `resource_types` of the kind currently browsed.
    pub current_type: Option<usize>,
    /// Objects of the current kind.
    pub rows: Vec<ResourceRow>,
    /// Indices into `rows` surviving `row_filter`.
    pub row_view: Vec<usize>,
    pub row_selected: usize,
    pub row_filter: Option<String>,
    pub rows_error: Option<String>,
    /// Set while a list request is in flight, so the pane can say so.
    pub loading: bool,

    pub right: RightMode,
    pub view: View,
    pub pane: Pane,
    pub mode: InputMode,
    pub input: String,
    pub palette: Palette,

    pub inventory: Inventory,

    // --------------------------------------------------------------- events
    pub events: Vec<EventInfo>,
    pub event_view: Vec<usize>,
    pub event_selected: usize,
    pub warnings_only: bool,
    pub events_error: Option<String>,

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
    /// Key of the object the current streams belong to, so switching rows
    /// tears the old pod's streams down instead of merging two pods' logs.
    attached_to: Option<String>,
    streams: HashMap<Arc<str>, tokio::task::JoinHandle<()>>,
    log_tx: Sender<LogEvent>,

    pub status: Status,
    pub dirty: bool,
    pub should_quit: bool,
    pub started: Instant,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        client: kube::Client,
        scope: Scope,
        log_tx: Sender<LogEvent>,
        context_name: String,
        user_name: String,
        k8s_version: String,
        contexts: Vec<ContextEntry>,
    ) -> Self {
        let highlighter = Highlighter::new(&config.theme, &config.highlight);
        let buffer = LogBuffer::new(config.logs.buffer_lines);
        let metrics = MetricsStore::new(config.metrics.history);
        let context_selected = contexts
            .iter()
            .position(|c| c.name == context_name)
            .unwrap_or(0);
        Self {
            follow: config.logs.follow,
            wrap: config.logs.wrap,
            timestamps: config.logs.timestamps,
            previous: false,
            config,
            client,
            scope,
            context_name,
            user_name,
            k8s_version,
            contexts,
            context_selected,
            pending_context: None,
            pollers_stale: false,
            resource_types: Vec::new(),
            current_type: None,
            rows: Vec::new(),
            row_view: Vec::new(),
            row_selected: 0,
            row_filter: None,
            rows_error: None,
            loading: false,
            right: RightMode::Browse,
            view: View::Logs,
            pane: Pane::Resources,
            mode: InputMode::Normal,
            input: String::new(),
            palette: Palette::default(),
            inventory: Inventory::default(),
            events: Vec::new(),
            event_view: Vec::new(),
            event_selected: 0,
            warnings_only: false,
            events_error: None,
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
            attached_to: None,
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

    // ------------------------------------------------------------- resources

    /// Hand the app the cluster's resource catalogue. Called once after
    /// connecting, and again after every context switch.
    pub fn on_resource_types(&mut self, types: Vec<ResourceType>) {
        let previous = self.current_type().map(|t| t.qualified());
        self.resource_types = types;
        // Keep browsing the same kind across a context switch when the new
        // cluster also serves it; otherwise fall back to pods.
        self.current_type = previous
            .and_then(|q| self.resource_types.iter().position(|t| t.qualified() == q))
            .or_else(|| self.resource_types.iter().position(|t| t.name() == "pods"));
        self.dirty = true;
    }

    pub fn current_type(&self) -> Option<&ResourceType> {
        self.current_type.and_then(|i| self.resource_types.get(i))
    }

    /// Name of the kind on screen, for titles and the status bar.
    pub fn resource_label(&self) -> String {
        self.current_type()
            .map(|t| t.name())
            .unwrap_or_else(|| "—".to_string())
    }

    /// Replace the browsed object list.
    pub fn on_rows(&mut self, rows: Result<Vec<ResourceRow>, String>) {
        self.loading = false;
        match rows {
            Ok(rows) => {
                self.rows_error = None;
                self.rows = rows;
                self.rebuild_row_view();
            }
            Err(err) => {
                self.rows_error = Some(err.clone());
                self.rows.clear();
                self.row_view.clear();
                self.set_status(err, StatusKind::Error);
            }
        }
        self.dirty = true;
    }

    fn rebuild_row_view(&mut self) {
        let previous = self.selected_row().map(|r| r.key());
        let needle = self.row_filter.as_deref().map(str::to_ascii_lowercase);
        self.row_view = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| match &needle {
                None => true,
                Some(n) => {
                    r.name.to_ascii_lowercase().contains(n)
                        || r.namespace.to_ascii_lowercase().contains(n)
                }
            })
            .map(|(i, _)| i)
            .collect();

        self.row_selected = previous
            .and_then(|key| {
                self.row_view
                    .iter()
                    .position(|&i| self.rows[i].key() == key)
            })
            .unwrap_or(0);
        self.row_selected = self.row_selected.min(self.row_view.len().saturating_sub(1));
        self.rebuild_event_view();
    }

    pub fn visible_rows(&self) -> impl Iterator<Item = &ResourceRow> {
        self.row_view.iter().filter_map(|&i| self.rows.get(i))
    }

    pub fn selected_row(&self) -> Option<&ResourceRow> {
        let &index = self.row_view.get(self.row_selected)?;
        self.rows.get(index)
    }

    /// The selected object as a pod, when the browsed kind *is* pods. Logs and
    /// container metrics only make sense in that case.
    pub fn selected_pod(&self) -> Option<&PodInfo> {
        if self.current_type()?.api.kind != "Pod" {
            return None;
        }
        let row = self.selected_row()?;
        self.inventory
            .pods
            .iter()
            .find(|p| p.name == row.name && p.namespace == row.namespace)
    }

    /// Ask the main loop to re-list the current kind.
    pub fn request_rows(&mut self) {
        self.loading = true;
        self.dirty = true;
    }

    fn set_resource_type(&mut self, selector: &str) {
        match crate::k8s::resources::resolve(&self.resource_types, selector) {
            Some(kind) => {
                let qualified = kind.qualified();
                self.current_type = self
                    .resource_types
                    .iter()
                    .position(|t| t.qualified() == qualified);
                self.rows.clear();
                self.row_view.clear();
                self.row_selected = 0;
                self.row_filter = None;
                self.rows_error = None;
                self.right = RightMode::Browse;
                self.pane = Pane::Resources;
                self.request_rows();
                self.set_status(format!("browsing {qualified}"), StatusKind::Info);
            }
            None => self.set_status(
                format!("no such resource type: {selector}"),
                StatusKind::Warn,
            ),
        }
    }

    // -------------------------------------------------------------- contexts

    fn switch_context(&mut self) {
        let Some(entry) = self.contexts.get(self.context_selected) else {
            return;
        };
        if entry.name == self.context_name {
            self.set_status(format!("already on {}", entry.name), StatusKind::Info);
            return;
        }
        let name = entry.name.clone();
        self.set_status(format!("switching to {name}…"), StatusKind::Info);
        self.pending_context = Some(name);
    }

    /// Wipe everything cluster-specific. The main loop calls this after it has
    /// built a client for the new context.
    pub fn adopt_context(
        &mut self,
        client: kube::Client,
        scope: Scope,
        context_name: String,
        user_name: String,
        k8s_version: String,
    ) {
        self.detach_all();
        self.client = client;
        self.scope = scope;
        self.context_name = context_name;
        self.user_name = user_name;
        self.k8s_version = k8s_version;
        self.inventory = Inventory::default();
        self.metrics = MetricsStore::new(self.config.metrics.history);
        self.events.clear();
        self.event_view.clear();
        self.events_error = None;
        self.rows.clear();
        self.row_view.clear();
        self.row_selected = 0;
        self.rows_error = None;
        self.buffer.clear();
        self.scroll = 0;
        self.right = RightMode::Browse;
        self.set_status(
            format!("connected to {}", self.context_name),
            StatusKind::Info,
        );
    }

    // ------------------------------------------------------------- inventory

    pub fn on_update(&mut self, update: Update) {
        match update {
            Update::Inventory(inv) => {
                self.inventory = *inv;
                self.reconcile_metrics_metadata();
                // The inventory can land after the user has already opened a
                // pod, in which case the first attach found nothing to do.
                self.sync_logs();
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

    // --------------------------------------------------------------- events

    pub fn on_events(&mut self, update: EventUpdate) {
        match update {
            EventUpdate::Events(events) => {
                self.events_error = None;
                self.events = events;
                self.rebuild_event_view();
                self.dirty = true;
            }
            EventUpdate::Unavailable(err) => {
                self.events_error = Some(err.clone());
                self.set_status(err, StatusKind::Error);
            }
        }
    }

    /// The events pane is always about the selected object.
    pub fn event_selector(&self) -> Option<String> {
        let kind = self.current_type()?.api.kind.to_ascii_lowercase();
        let row = self.selected_row()?;
        Some(format!("{kind}/{}", row.name))
    }

    pub fn rebuild_event_view(&mut self) {
        let selector = self.event_selector();
        self.event_view = self
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| !self.warnings_only || e.is_warning())
            .filter(|(_, e)| match &selector {
                Some(s) => e.matches_resource(s),
                None => true,
            })
            .map(|(i, _)| i)
            .collect();
        self.event_selected = self
            .event_selected
            .min(self.event_view.len().saturating_sub(1));
    }

    pub fn visible_events(&self) -> impl Iterator<Item = &EventInfo> {
        self.event_view.iter().filter_map(|&i| self.events.get(i))
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
            LogEvent::Ended(source) => self.set_status(
                format!("{source} stream ended — retrying"),
                StatusKind::Warn,
            ),
            LogEvent::Failed { source, error } => {
                self.set_status(format!("{source}: {error}"), StatusKind::Error)
            }
        }
    }

    /// Make the log streams match whatever the logs tab is currently showing.
    ///
    /// Called every time the logs tab becomes visible or the selection moves,
    /// so the buffer always belongs to exactly one object. A no-op when the
    /// streams already point at the right pod.
    pub fn sync_logs(&mut self) {
        if self.right != RightMode::Detail || self.view != View::Logs {
            return;
        }
        let wanted = self.selected_row().map(|r| r.key());
        if wanted.is_some() && wanted == self.attached_to {
            return;
        }
        self.detach_all();
        self.buffer.clear();
        self.scroll = 0;
        self.search.current = None;
        self.attach_selected();
    }

    /// Attach to every container of the selected pod.
    pub fn attach_selected(&mut self) {
        let Some(pod) = self.selected_pod().cloned() else {
            let hint = match self.current_type() {
                Some(t) if t.api.kind != "Pod" => {
                    format!(
                        "logs are only available for pods — press :pods (this is {})",
                        t.name()
                    )
                }
                _ => "waiting for the pod inventory…".to_string(),
            };
            self.set_status(hint, StatusKind::Warn);
            return;
        };
        self.attached_to = self.selected_row().map(|r| r.key());
        for container in pod.containers.iter().filter(|c| !c.init) {
            let spec = StreamSpec {
                namespace: pod.namespace.clone(),
                pod: pod.name.clone(),
                container: Some(container.name.clone()),
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
        self.attached_to = None;
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
            SortBy::Name => rows.sort_by_key(|a| a.key()),
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
        // The palette has its own key loop: Tab and the arrows drive the
        // completion list rather than the prompt text.
        if self.mode == InputMode::Command {
            self.on_palette_key(key);
            return;
        }
        if matches!(
            self.mode,
            InputMode::Search | InputMode::Filter | InputMode::Namespace | InputMode::RowFilter
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
                KeyCode::Char('r') => self.request_rows(),
                KeyCode::Char('l') => self.dirty = true,
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.on_escape(),
            KeyCode::Char('?') => {
                self.mode = InputMode::Help;
                self.dirty = true;
            }
            KeyCode::Char(':') => self.open_palette(),

            // panes and detail tabs
            KeyCode::Tab => {
                self.pane = match self.pane {
                    Pane::Contexts => Pane::Resources,
                    Pane::Resources => Pane::Contexts,
                };
                self.dirty = true;
            }
            KeyCode::Char('1') => self.set_view(View::Logs),
            KeyCode::Char('2') => self.set_view(View::Metrics),
            KeyCode::Char('3') => self.set_view(View::Events),

            // navigation
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            // → / ← page as well as PgDn/PgUp; horizontal scrolling is on [ ].
            KeyCode::PageDown | KeyCode::Right => self.move_cursor(self.viewport_height as isize),
            KeyCode::PageUp | KeyCode::Left => self.move_cursor(-(self.viewport_height as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.goto_start(),
            KeyCode::Char('G') | KeyCode::End => self.goto_end(),
            KeyCode::Char('[') => {
                self.h_scroll = self.h_scroll.saturating_sub(8);
                self.dirty = true;
            }
            KeyCode::Char(']') => {
                self.h_scroll = (self.h_scroll + 8).min(4096);
                self.dirty = true;
            }

            KeyCode::Enter => self.activate_selection(),
            KeyCode::Char('/') => self.enter_prompt(match self.right {
                RightMode::Browse => InputMode::RowFilter,
                RightMode::Detail => InputMode::Search,
            }),

            // log controls
            KeyCode::Char('\\') => self.enter_prompt(InputMode::Filter),
            KeyCode::Char('n') => self.search_next(false),
            KeyCode::Char('N') => self.search_next(true),
            KeyCode::Char('x') => {
                self.detach_all();
                self.set_status("detached from all streams", StatusKind::Info);
            }
            KeyCode::Char('F') => {
                self.follow = !self.follow;
                if self.follow {
                    self.scroll_to_bottom();
                }
                let msg = if self.follow {
                    "follow on"
                } else {
                    "follow off"
                };
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

            KeyCode::Char('W') if self.view == View::Events => {
                self.warnings_only = !self.warnings_only;
                self.rebuild_event_view();
                self.set_status(
                    if self.warnings_only {
                        "showing warnings only"
                    } else {
                        "showing all events"
                    },
                    StatusKind::Info,
                );
            }

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

    /// `Esc` steps back out: detail → browse → quit.
    fn on_escape(&mut self) {
        match self.right {
            RightMode::Detail => {
                self.right = RightMode::Browse;
                self.dirty = true;
            }
            RightMode::Browse => self.should_quit = true,
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
                    InputMode::RowFilter => {
                        self.row_filter = if value.trim().is_empty() {
                            None
                        } else {
                            Some(value.trim().to_string())
                        };
                        self.rebuild_row_view();
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

    /// The `:` palette over resource types. `Tab` completes into the prompt,
    /// the arrows move the highlight, and `Enter` accepts it — so `:dep<Enter>`
    /// lands on `deployments` the way it does in k9s.
    fn on_palette_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.close_palette(),
            KeyCode::Enter => {
                let chosen = self
                    .palette
                    .current()
                    .map(str::to_string)
                    .unwrap_or_else(|| self.input.clone());
                self.close_palette();
                if !chosen.trim().is_empty() {
                    self.set_resource_type(&chosen);
                }
            }
            KeyCode::Tab => {
                if let Some(completion) = self.palette.current().map(str::to_string) {
                    if completion == self.input {
                        self.palette.move_selection(1);
                    } else {
                        self.input = completion;
                        let input = self.input.clone();
                        self.palette.refilter(&input);
                    }
                }
                self.dirty = true;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.palette.move_selection(-1);
                self.dirty = true;
            }
            KeyCode::Down => {
                self.palette.move_selection(1);
                self.dirty = true;
            }
            KeyCode::Backspace => {
                self.input.pop();
                let input = self.input.clone();
                self.palette.refilter(&input);
                self.dirty = true;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                let input = self.input.clone();
                self.palette.refilter(&input);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn open_palette(&mut self) {
        self.mode = InputMode::Command;
        self.input.clear();
        // One entry per kind, displayed by its canonical plural but matched on
        // every alias too, so `pvc` reaches persistentvolumeclaims instead of
        // fuzzy-matching some unrelated plural.
        let candidates: Vec<Candidate> = self
            .resource_types
            .iter()
            .map(|t| Candidate::with_keys(t.name(), t.aliases()))
            .collect();
        self.palette.reload(candidates, "");
        self.dirty = true;
    }

    fn close_palette(&mut self) {
        self.mode = InputMode::Normal;
        self.input.clear();
        self.dirty = true;
    }

    /// Incremental feedback while typing a search or row filter.
    fn live_preview(&mut self) {
        match self.mode {
            InputMode::Search => {
                self.search.query = self.input.clone();
                self.search.regex = self.compile(&self.input);
                self.refresh_search_count();
            }
            InputMode::RowFilter => {
                self.row_filter = if self.input.is_empty() {
                    None
                } else {
                    Some(self.input.clone())
                };
                self.rebuild_row_view();
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
        // Picking a tab is also how you open the detail pane.
        if self.right == RightMode::Browse && self.selected_row().is_some() {
            self.right = RightMode::Detail;
        }
        // Unconditional: arriving on the logs tab from *any* other tab has to
        // attach, not just the Browse → Detail transition.
        self.sync_logs();
        self.dirty = true;
    }

    fn set_namespace(&mut self, value: &str) {
        let value = value.trim();
        self.scope = if value.is_empty() || value == "all" || value == "*" {
            Scope::AllNamespaces
        } else {
            Scope::Namespace(value.to_string())
        };
        self.request_rows();
        // Inventory, metrics and events all capture the scope when they are
        // spawned, so they have to come back up against the new one.
        self.pollers_stale = true;
        self.set_status(
            format!("namespace scope: {}", self.scope.label()),
            StatusKind::Info,
        );
    }

    fn move_cursor(&mut self, delta: isize) {
        match (self.pane, self.right) {
            (Pane::Contexts, _) => {
                let len = self.contexts.len();
                if len == 0 {
                    return;
                }
                self.context_selected =
                    (self.context_selected as isize + delta).clamp(0, len as isize - 1) as usize;
                self.dirty = true;
            }
            (Pane::Resources, RightMode::Browse) => {
                let len = self.row_view.len();
                if len == 0 {
                    return;
                }
                self.row_selected =
                    (self.row_selected as isize + delta).clamp(0, len as isize - 1) as usize;
                // The detail tabs follow the browser's selection.
                self.rebuild_event_view();
                self.dirty = true;
            }
            (Pane::Resources, RightMode::Detail) => match self.view {
                View::Logs => self.scroll_by(delta),
                View::Events => {
                    let len = self.event_view.len();
                    if len > 0 {
                        self.event_selected = (self.event_selected as isize + delta)
                            .clamp(0, len as isize - 1)
                            as usize;
                    }
                    self.dirty = true;
                }
                View::Metrics => {
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
                                self.pod_metric_selected = (self.pod_metric_selected as isize
                                    + delta)
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
            },
        }
    }

    fn goto_start(&mut self) {
        match (self.pane, self.right) {
            (Pane::Contexts, _) => self.context_selected = 0,
            (Pane::Resources, RightMode::Browse) => {
                self.row_selected = 0;
                self.rebuild_event_view();
            }
            (Pane::Resources, RightMode::Detail) => {
                self.follow = false;
                self.scroll = 0;
            }
        }
        self.dirty = true;
    }

    fn goto_end(&mut self) {
        match (self.pane, self.right) {
            (Pane::Contexts, _) => {
                self.context_selected = self.contexts.len().saturating_sub(1);
            }
            (Pane::Resources, RightMode::Browse) => {
                self.row_selected = self.row_view.len().saturating_sub(1);
                self.rebuild_event_view();
            }
            (Pane::Resources, RightMode::Detail) => {
                self.follow = true;
                self.scroll_to_bottom();
            }
        }
        self.dirty = true;
    }

    fn activate_selection(&mut self) {
        match self.pane {
            Pane::Contexts => self.switch_context(),
            Pane::Resources => {
                if self.right == RightMode::Browse && self.selected_row().is_some() {
                    self.right = RightMode::Detail;
                    self.sync_logs();
                    self.dirty = true;
                }
            }
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
