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
use crate::k8s::fleet::{Fleet, Listing};
use crate::k8s::logs::{LogEvent, StreamSpec};
use crate::k8s::metrics::MetricsEvent;
use crate::k8s::resources::{ResourceRow, ResourceType};
use crate::k8s::selection::{LabelSelector, LogTarget};
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
    /// The object on every member of the fleet, side by side.
    Compare,
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
    /// `-l` label selector entry.
    Selector,
    /// Context globs for an ad-hoc fleet, typed on the contexts pane.
    Fleet,
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
            InputMode::Selector => "selector (k=v,k2!=v2) > ",
            InputMode::Fleet => "fleet (name or globs, e.g. stg-*,qa-eu) > ",
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

/// What to open once a context switch requested from a fleet row lands.
#[derive(Debug, Clone)]
struct PendingOpen {
    /// [`ResourceRow::identity`] of the row.
    identity: String,
    /// The detail tab to open, or `None` to stay in the list.
    view: Option<View>,
    describe: bool,
}

/// Pods wanted from the other members of a fleet, for logs merged across
/// it. Taken by the main loop, answered through [`App::on_fleet_pods`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetPodsRequest {
    /// The log key the answer belongs to; a later key makes it stale.
    pub key: String,
    pub members: Vec<String>,
    pub namespace: String,
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
    /// Fleets from the config file that match at least one context, listed
    /// above the contexts.
    pub fleets: Vec<Fleet>,
    /// The fleet being browsed, if any: the list then spans every member,
    /// while logs, metrics and describe still talk to the one connected
    /// context, switching to a row's context when it is opened.
    pub fleet: Option<Fleet>,
    /// Members that did not answer the last fleet listing.
    pub fleet_failed: Vec<(String, String)>,
    pending_open: Option<PendingOpen>,
    /// `--kind`: the type to land on once discovery has answered.
    pub start_kind: Option<String>,
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
    /// `-l`/`--selector`: narrows both the browsed list and the pods a
    /// workload's logs are collected from.
    pub selector: LabelSelector,
    /// Raw selector text, for the header and for re-editing.
    pub selector_text: String,
    /// Show only rows whose status reads as a problem — the triage filter.
    pub problems_only: bool,
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
    /// Whether the connected context's first inventory snapshot has arrived.
    /// Until it has, "no pods behind this object" would be a false alarm.
    pub inventory_ready: bool,

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

    // -------------------------------------------------------- volume usage
    /// Live PVC usage from the kubelets, keyed `namespace/claim`.
    pub volume_usage: std::collections::HashMap<String, crate::k8s::volume_stats::VolumeUsage>,
    /// Why the usage columns are empty, when they are.
    pub volume_usage_error: Option<String>,
    volume_usage_at: Option<Instant>,
    volume_usage_pending: bool,

    // ------------------------------------------------------------- describe
    /// The describe tab is showing. It sits beside logs, metrics and events,
    /// so picking any of those leaves it.
    pub describe_open: bool,
    /// The compare tab: the open object as each fleet member has it.
    pub compare: Vec<crate::compare::Member>,
    /// Which object (and fleet) `compare` belongs to.
    compare_for: Option<String>,
    /// Set when the compare tab needs fetching; taken by the main loop.
    compare_wanted: bool,
    /// A fetch is in flight.
    pub compare_loading: bool,
    /// The fetched object, kept so `v` can re-render it with or without
    /// Secret values without asking the API server again.
    describe_object: Option<serde_json::Value>,
    /// `v` revealed this Secret's values. Reset for every new object.
    pub secrets_revealed: bool,
    /// Rendered YAML of the open object, and which object it belongs to.
    pub describe_text: String,
    /// Byte range of every line in `describe_text`, computed once per fetch.
    describe_lines: Vec<std::ops::Range<usize>>,
    pub describe_scroll: usize,
    pub describe_error: Option<String>,
    describe_for: Option<String>,
    pub describe_loading: bool,

    /// Which node service the logs tab is showing when a Node is open.
    pub node_service: usize,
    /// Set while a node log request is in flight.
    pub node_logs_loading: bool,
    /// Node whose logs were last requested, so the fetch is not repeated on
    /// every inventory tick.
    node_logs_for: Option<String>,

    pub attached: Vec<Arc<str>>,
    /// Key of the object the current streams belong to, so switching rows
    /// tears the old pod's streams down instead of merging two pods' logs.
    attached_to: Option<String>,
    /// Every open stream with the spec it was opened from, so a toggle
    /// (timestamps, previous) can reopen exactly the same set.
    streams: HashMap<Arc<str>, (StreamSpec, tokio::task::JoinHandle<()>)>,
    /// While logs span a fleet: the members the open object lives on.
    pub log_members: Vec<String>,
    /// Pods (and a client) per other member, for fleet-wide logs.
    fleet_pods: HashMap<String, (kube::Client, Vec<PodInfo>)>,
    fleet_pods_request: Option<FleetPodsRequest>,
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
        let names: Vec<String> = contexts.iter().map(|c| c.name.clone()).collect();
        let fleets = Fleet::all_configured(&config.fleets, &names);
        let context_selected = fleets.len()
            + contexts
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
            fleets,
            fleet: None,
            fleet_failed: Vec::new(),
            pending_open: None,
            start_kind: None,
            pollers_stale: false,
            resource_types: Vec::new(),
            current_type: None,
            rows: Vec::new(),
            row_view: Vec::new(),
            row_selected: 0,
            row_filter: None,
            selector: LabelSelector::default(),
            selector_text: String::new(),
            problems_only: false,
            rows_error: None,
            loading: false,
            right: RightMode::Browse,
            view: View::Logs,
            pane: Pane::Resources,
            mode: InputMode::Normal,
            input: String::new(),
            palette: Palette::default(),
            inventory: Inventory::default(),
            inventory_ready: false,
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
            volume_usage: std::collections::HashMap::new(),
            volume_usage_error: None,
            volume_usage_at: None,
            volume_usage_pending: false,
            describe_open: false,
            describe_text: String::new(),
            describe_object: None,
            compare: Vec::new(),
            compare_for: None,
            compare_wanted: false,
            compare_loading: false,
            secrets_revealed: false,
            describe_lines: Vec::new(),
            describe_scroll: 0,
            describe_error: None,
            describe_for: None,
            describe_loading: false,
            node_service: 0,
            node_logs_loading: false,
            node_logs_for: None,
            attached: Vec::new(),
            attached_to: None,
            streams: HashMap::new(),
            log_members: Vec::new(),
            fleet_pods: HashMap::new(),
            fleet_pods_request: None,
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
        if let Some(kind) = self.start_kind.take() {
            self.set_resource_type(&kind);
        }
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

    /// Take a listing: one context's rows, or a fleet's with the members that
    /// failed. A fleet only errors out when no member answered at all.
    pub fn on_listing(&mut self, listing: Result<Listing, String>) {
        let listing = match listing {
            Ok(listing) => listing,
            Err(err) => return self.on_rows(Err(err)),
        };
        self.fleet_failed = listing.failed;
        let members = self.fleet.as_ref().map_or(1, |f| f.members.len());
        if !self.fleet_failed.is_empty() && self.fleet_failed.len() == members {
            let (context, err) = &self.fleet_failed[0];
            return self.on_rows(Err(format!("no fleet member answered ({context}: {err})")));
        }
        self.on_rows(Ok(listing.rows));
        if let Some((context, err)) = self.fleet_failed.first() {
            let more = match self.fleet_failed.len() {
                1 => String::new(),
                n => format!(" (+{} more)", n - 1),
            };
            self.set_status(
                format!("{context} unreachable{more}: {err}"),
                StatusKind::Warn,
            );
        }
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
        let previous = self.selected_row().map(|r| r.identity());
        let needle = self.row_filter.as_deref().map(str::to_ascii_lowercase);
        self.row_view = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !self.problems_only || r.unhealthy)
            .filter(|(_, r)| match &needle {
                None => true,
                Some(n) => {
                    r.name.to_ascii_lowercase().contains(n)
                        || r.namespace.to_ascii_lowercase().contains(n)
                        || r.context.to_ascii_lowercase().contains(n)
                }
            })
            .map(|(i, _)| i)
            .collect();

        self.row_selected = previous
            .and_then(|key| {
                self.row_view
                    .iter()
                    .position(|&i| self.rows[i].identity() == key)
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

    /// The selected row's context when it is not the connected one. Nothing
    /// may be fetched for such a row until kscope has switched to it.
    pub fn remote_context(&self) -> Option<&str> {
        let row = self.selected_row()?;
        (!row.context.is_empty() && row.context != self.context_name)
            .then_some(row.context.as_str())
    }

    /// Opening a row that lives on another member: switch there first and
    /// finish the job in [`App::adopt_context`]. Returns false when the row
    /// is local and the caller should go ahead.
    fn open_remote(&mut self, view: Option<View>, describe: bool) -> bool {
        let Some(context) = self.remote_context().map(str::to_string) else {
            return false;
        };
        // Logs are merged from every member in place, and compare reads
        // every member anyway: neither needs a switch.
        if !describe
            && (view == Some(View::Compare)
                || (view == Some(View::Logs) && self.fleet_log_target().is_some()))
        {
            return false;
        }
        let Some(row) = self.selected_row() else {
            return false;
        };
        let (identity, key) = (row.identity(), row.key());
        self.pending_open = Some(PendingOpen {
            identity,
            view,
            describe,
        });
        self.set_status(
            format!("switching to {context} for {key}…"),
            StatusKind::Info,
        );
        self.pending_context = Some(context);
        true
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

    /// Entries in the left-hand pane: fleets, then contexts. One cursor
    /// (`context_selected`) runs over both.
    pub fn pane_len(&self) -> usize {
        self.fleets.len() + self.contexts.len()
    }

    fn switch_context(&mut self) {
        let index = self.context_selected;
        if let Some(fleet) = self.fleets.get(index) {
            let fleet = fleet.clone();
            return self.enter_fleet(fleet);
        }
        let Some(entry) = self.contexts.get(index - self.fleets.len()) else {
            return;
        };
        let name = entry.name.clone();
        // Picking a single context is how you leave a fleet.
        let left_fleet = self.fleet.take().is_some();
        if name == self.context_name {
            if left_fleet {
                self.reset_rows();
                self.request_rows();
                self.set_status(format!("left the fleet, browsing {name}"), StatusKind::Info);
            } else {
                self.set_status(format!("already on {name}"), StatusKind::Info);
            }
            return;
        }
        self.pending_open = None;
        self.set_status(format!("switching to {name}…"), StatusKind::Info);
        self.pending_context = Some(name);
    }

    /// Browse `fleet`: list the current kind on every member. The connected
    /// context stays as it is until a row from another member is opened.
    ///
    /// Picked in the TUI, a fleet spans every namespace — it is for finding
    /// things wherever they are, and `Ctrl-n` narrows it. Started from the
    /// command line, it keeps the scope `-n`/`-A` gave it.
    pub fn enter_fleet(&mut self, fleet: Fleet) {
        if self.scope != Scope::AllNamespaces {
            self.scope = Scope::AllNamespaces;
            self.pollers_stale = true;
        }
        self.open_fleet(fleet);
    }

    /// [`App::enter_fleet`] without touching the namespace scope.
    pub fn open_fleet(&mut self, fleet: Fleet) {
        self.set_status(
            format!(
                "fleet {}: {} ({} contexts)",
                fleet.name,
                fleet.members.join(", "),
                fleet.members.len()
            ),
            StatusKind::Info,
        );
        self.fleet = Some(fleet);
        self.fleet_failed.clear();
        self.reset_rows();
        self.right = RightMode::Browse;
        self.pane = Pane::Resources;
        self.request_rows();
    }

    fn apply_fleet_input(&mut self, raw: &str) {
        let names: Vec<String> = self.contexts.iter().map(|c| c.name.clone()).collect();
        match Fleet::resolve(raw, &self.config.fleets, &names) {
            Some(fleet) => self.enter_fleet(fleet),
            None => self.set_status(
                format!("no context matches {:?}", raw.trim()),
                StatusKind::Warn,
            ),
        }
    }

    fn reset_rows(&mut self) {
        self.rows.clear();
        self.row_view.clear();
        self.row_selected = 0;
        self.rows_error = None;
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
        self.inventory_ready = false;
        self.metrics = MetricsStore::new(self.config.metrics.history);
        // The pane's cursor follows the cluster actually connected to, so a
        // switch made from a fleet row is visible on the left too.
        if let Some(i) = self
            .contexts
            .iter()
            .position(|c| c.name == self.context_name)
        {
            self.context_selected = self.fleets.len() + i;
        }
        self.events.clear();
        self.event_view.clear();
        self.events_error = None;
        // Describe is keyed by kind/namespace/name, which the new cluster
        // may well have too: never show the old cluster's copy.
        self.describe_open = false;
        self.describe_for = None;
        self.describe_object = None;
        self.secrets_revealed = false;
        self.describe_text.clear();
        self.describe_lines.clear();
        self.describe_error = None;
        self.buffer.clear();
        self.scroll = 0;
        self.right = RightMode::Browse;
        self.set_status(
            format!("connected to {}", self.context_name),
            StatusKind::Info,
        );
        // Within a fleet the list is still valid — it spans every member —
        // so keep it, and open whatever the switch was for.
        if self.fleet.is_none() {
            self.reset_rows();
            self.pending_open = None;
            return;
        }
        let Some(open) = self.pending_open.take() else {
            return;
        };
        let Some(position) = self
            .row_view
            .iter()
            .position(|&i| self.rows[i].identity() == open.identity)
        else {
            return;
        };
        self.row_selected = position;
        self.rebuild_event_view();
        if let Some(view) = open.view {
            self.set_view(view);
        }
        if open.describe {
            self.open_describe();
        }
    }

    // ------------------------------------------------------------- inventory

    pub fn on_update(&mut self, update: Update) {
        match update {
            Update::Inventory(inv) => {
                self.inventory = *inv;
                self.inventory_ready = true;
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
                // Keep the hit count incremental: rescanning the whole buffer
                // on every batch made an active search O(buffer) per batch.
                let re = self.search.regex.as_ref();
                let mut total = self.search.total;
                // Lines evicted from the front of the view shift every view
                // index down by one; positions held across the batch have to
                // shift with them or they point at the wrong line.
                let mut shifted = 0usize;
                for line in lines {
                    let line = LogLine::new(line, source.clone());
                    let hit = re.is_some_and(|re| re.is_match(&line.raw));
                    let pushed = self.buffer.push(line);
                    if pushed.shown && hit {
                        total += 1;
                    }
                    if let Some(old) = pushed.evicted {
                        shifted += 1;
                        if re.is_some_and(|re| re.is_match(&old.raw)) {
                            total = total.saturating_sub(1);
                        }
                    }
                }
                self.search.total = total;
                if shifted > 0 {
                    // The current match is gone if it was among the evicted.
                    self.search.current = self.search.current.and_then(|i| i.checked_sub(shifted));
                    if !self.follow {
                        self.scroll = self.scroll.saturating_sub(shifted);
                    }
                }
                if self.follow {
                    self.scroll_to_bottom();
                }
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
    /// Usage of one claim, if a kubelet has reported it.
    pub fn usage_for(
        &self,
        namespace: &str,
        name: &str,
    ) -> Option<&crate::k8s::volume_stats::VolumeUsage> {
        self.volume_usage.get(&format!("{namespace}/{name}"))
    }

    /// Whether usage has been asked for at least once, so the UI can tell
    /// "not fetched yet" apart from "fetched, and the driver reports nothing".
    pub fn volume_usage_polled(&self) -> bool {
        self.volume_usage_at.is_some()
    }

    /// True when the volumes table is on screen and its usage figures are due
    /// a refresh. Polled rather than streamed: this is one request per node,
    /// so it runs on the metrics interval and only while it is being looked at.
    pub fn take_volume_usage_request(&mut self) -> bool {
        let looking = self.right == RightMode::Detail
            && self.view == View::Metrics
            && self.metric_pane == MetricPane::Volumes;
        if !looking || self.volume_usage_pending || self.inventory.nodes.is_empty() {
            return false;
        }
        let due = self
            .volume_usage_at
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(self.config.metrics.refresh_ms));
        if !due {
            return false;
        }
        self.volume_usage_pending = true;
        true
    }

    pub fn on_volume_usage(
        &mut self,
        result: Result<
            std::collections::HashMap<String, crate::k8s::volume_stats::VolumeUsage>,
            String,
        >,
    ) {
        self.volume_usage_pending = false;
        self.volume_usage_at = Some(Instant::now());
        match result {
            Ok(usage) => {
                self.volume_usage = usage;
                self.volume_usage_error = None;
            }
            Err(err) => self.volume_usage_error = Some(err),
        }
        self.dirty = true;
    }

    /// Open or close the describe tab for the object in hand.
    fn toggle_describe(&mut self) {
        if self.describe_open {
            self.describe_open = false;
            self.dirty = true;
            return;
        }
        // Describe works straight from the list too — you should not have to
        // open an object first just to look at its YAML.
        if self.selected_row().is_none() {
            self.set_status("nothing selected", StatusKind::Warn);
            return;
        }
        let view = (self.right == RightMode::Detail).then_some(self.view);
        if self.open_remote(view, true) {
            return;
        }
        self.open_describe();
    }

    /// Describe is a tab like logs, metrics and events: it opens the detail
    /// pane, so `l`, `m` and `e` lead straight on from it.
    fn open_describe(&mut self) {
        self.describe_open = true;
        if self.selected_row().is_some() {
            self.right = RightMode::Detail;
        }
        self.sync_describe();
        self.dirty = true;
    }

    /// Ask for the open object's full YAML, unless it is already rendered.
    fn sync_describe(&mut self) {
        if !self.describe_open {
            return;
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        let Some(kind) = self.current_type() else {
            return;
        };
        let key = format!("{}/{}/{}", kind.qualified(), row.namespace, row.name);
        if let Some(context) = self.remote_context() {
            // Moving the cursor with describe open onto another member's row.
            self.describe_error = Some(format!(
                "{} is on {context} — press Enter or d to switch there",
                row.key()
            ));
            self.describe_for = None;
            self.describe_object = None;
            self.describe_text.clear();
            self.describe_lines.clear();
            self.describe_loading = false;
            self.dirty = true;
            return;
        }
        if self.describe_for.as_deref() == Some(key.as_str()) {
            return;
        }
        self.describe_for = Some(key);
        self.describe_object = None;
        self.secrets_revealed = false;
        self.describe_text.clear();
        self.describe_lines.clear();
        self.describe_error = None;
        self.describe_scroll = 0;
        self.describe_loading = true;
        self.dirty = true;
    }

    /// The pending describe request, taken by the main loop.
    pub fn take_describe_request(&mut self) -> Option<(ResourceType, String, String)> {
        if !self.describe_loading {
            return None;
        }
        self.describe_loading = false;
        if self.remote_context().is_some() {
            return None;
        }
        let kind = self.current_type()?.clone();
        let row = self.selected_row()?;
        Some((kind, row.namespace.clone(), row.name.clone()))
    }

    pub fn on_describe(&mut self, result: Result<serde_json::Value, String>) {
        match result {
            Ok(object) => {
                self.describe_object = Some(object);
                self.render_describe();
                self.describe_error = None;
            }
            Err(err) => {
                // Let the next visit retry rather than caching the failure.
                self.describe_for = None;
                self.describe_error = Some(err);
            }
        }
        self.describe_scroll = 0;
        self.dirty = true;
    }

    fn render_describe(&mut self) {
        let Some(object) = &self.describe_object else {
            return;
        };
        let reveal = self.config.describe.show_secret_values || self.secrets_revealed;
        self.describe_text = crate::describe::render(object, reveal);
        self.describe_lines = line_spans(&self.describe_text);
    }

    /// `v` on a Secret's describe: show its values decoded, or hide them again.
    fn toggle_secret_values(&mut self) {
        let is_secret = self
            .describe_object
            .as_ref()
            .is_some_and(crate::describe::has_secret_values);
        if !is_secret {
            self.set_status("v reveals values on a Secret only", StatusKind::Info);
            return;
        }
        if self.config.describe.show_secret_values {
            self.set_status(
                "secret values are always shown (describe.show_secret_values)",
                StatusKind::Info,
            );
            return;
        }
        if !self.config.describe.allow_reveal {
            self.set_status(
                "revealing secret values is turned off (describe.allow_reveal)",
                StatusKind::Warn,
            );
            return;
        }
        self.secrets_revealed = !self.secrets_revealed;
        self.render_describe();
        self.set_status(
            if self.secrets_revealed {
                "showing secret values, decoded — v to hide"
            } else {
                "secret values hidden — v to show"
            },
            StatusKind::Info,
        );
        self.dirty = true;
    }

    /// Number of lines in the rendered object.
    pub fn describe_len(&self) -> usize {
        self.describe_lines.len()
    }

    /// Lines `start..start + len` of the rendered object, without walking the
    /// text from the top on every frame.
    pub fn describe_window(&self, start: usize, len: usize) -> impl Iterator<Item = &str> {
        let end = (start + len).min(self.describe_lines.len());
        let start = start.min(end);
        self.describe_lines[start..end]
            .iter()
            .map(|r| self.describe_text[r.clone()].trim_end_matches('\r'))
    }

    /// The node service the logs tab is showing (`kubelet`, `containerd`, …).
    pub fn node_service(&self) -> &'static str {
        crate::k8s::node_logs::COMMON_SERVICES
            [self.node_service % crate::k8s::node_logs::COMMON_SERVICES.len()]
    }

    /// Ask for a node's journal, unless it is already on screen. Unlike pod
    /// logs this is a one-shot fetch, not a stream: the kubelet endpoint has
    /// no follow mode.
    fn sync_node_logs(&mut self, node: &str) {
        let key = format!("node/{node}/{}", self.node_service());
        if self.node_logs_for.as_deref() == Some(key.as_str()) {
            return;
        }
        self.detach_all();
        self.buffer.clear();
        self.scroll = 0;
        self.search.current = None;
        self.attached_to = None;
        self.node_logs_for = Some(key);
        self.node_logs_loading = true;
        self.dirty = true;
    }

    /// The pending node log request, taken by the main loop.
    pub fn take_node_log_request(&mut self) -> Option<(String, &'static str)> {
        if !self.node_logs_loading {
            return None;
        }
        self.node_logs_loading = false;
        let target = self.log_target()?;
        if !target.kind.eq_ignore_ascii_case("node") {
            return None;
        }
        Some((target.name, self.node_service()))
    }

    /// Deliver fetched node logs into the buffer.
    pub fn on_node_logs(&mut self, node: String, service: &str, result: Result<String, String>) {
        match result {
            Ok(text) => {
                let source: Arc<str> = Arc::from(format!("{node}:{service}"));
                for line in text.lines() {
                    self.buffer
                        .push(LogLine::new(line.to_string(), source.clone()));
                }
                self.attached = vec![source];
                self.scroll_to_bottom();
                self.refresh_search_count();
                self.set_status(
                    format!("{service} logs from {node} ({} lines)", self.buffer.len()),
                    StatusKind::Info,
                );
            }
            Err(err) => {
                // Leave the reason on screen: it names the feature gate or the
                // missing RBAC verb, which is the whole diagnosis.
                self.node_logs_for = None;
                self.set_status(err, StatusKind::Error);
            }
        }
        self.dirty = true;
    }

    /// The object the logs tab is about, as a selection target.
    pub fn log_target(&self) -> Option<LogTarget> {
        if self.remote_context().is_some() {
            return None;
        }
        let kind = self.current_type()?.api.kind.clone();
        let row = self.selected_row()?;
        Some(LogTarget {
            kind,
            namespace: row.namespace.clone(),
            name: row.name.clone(),
            selector: row.selector.clone(),
        })
    }

    /// Make the log streams match what the logs tab is showing — including as
    /// the cluster changes underneath it.
    ///
    /// This is what lets kscope follow a workload rather than a pod: opening a
    /// Deployment merges every replica into one buffer, and because the
    /// inventory poller calls this on every refresh, a rollout hands the stream
    /// over to the new pods instead of going silent on the old ones.
    pub fn sync_logs(&mut self) {
        if self.right != RightMode::Detail || self.view != View::Logs {
            return;
        }
        // Within a fleet a workload's logs span every member it runs on.
        let fleet = self.fleet_log_target();
        let Some(target) = fleet
            .as_ref()
            .map(|(target, _)| target.clone())
            .or_else(|| self.log_target())
        else {
            return;
        };

        // A Node has no pods of its own worth merging — it has a journal.
        if target.kind.eq_ignore_ascii_case("node") {
            self.sync_node_logs(&target.name);
            return;
        }

        let mut key = format!("{}/{}/{}", target.kind, target.namespace, target.name);
        if let (Some(_), Some(f)) = (&fleet, &self.fleet) {
            key = format!("fleet:{}|{key}", f.name);
        }

        // A different object means a different buffer.
        if self.attached_to.as_deref() != Some(key.as_str()) {
            self.detach_all();
            self.buffer.clear();
            self.scroll = 0;
            self.search.current = None;
            self.attached_to = Some(key.clone());
            self.fleet_pods.clear();
            self.log_members = fleet.as_ref().map(|(_, m)| m.clone()).unwrap_or_default();
            let others: Vec<String> = self
                .log_members
                .iter()
                .filter(|m| **m != self.context_name)
                .cloned()
                .collect();
            if !others.is_empty() {
                self.fleet_pods_request = Some(FleetPodsRequest {
                    key: key.clone(),
                    members: others,
                    namespace: pods_namespace(&target),
                });
            }
        }

        let template = StreamSpec {
            namespace: String::new(),
            pod: String::new(),
            container: None,
            tail: self.tail_lines(),
            since_seconds: self.config.logs.since_seconds,
            timestamps: self.timestamps,
            previous: self.previous,
            context: None,
        };
        let specs_for = |pods: Vec<&PodInfo>, context: Option<&str>| -> Vec<StreamSpec> {
            pods.into_iter()
                .flat_map(|pod| {
                    pod.containers
                        .iter()
                        .filter(|c| !c.init)
                        .map(|container| StreamSpec {
                            namespace: pod.namespace.clone(),
                            pod: pod.name.clone(),
                            container: Some(container.name.clone()),
                            context: context.map(str::to_string),
                            ..template.clone()
                        })
                })
                .collect()
        };
        let pods_for = crate::k8s::selection::pods_for;
        let mut wanted: Vec<StreamSpec> = Vec::new();
        if fleet.is_none() {
            let pods = pods_for(&target, &self.inventory.pods, &self.selector);
            wanted = specs_for(pods, None);
        } else {
            for member in &self.log_members {
                let pods = if *member == self.context_name {
                    &self.inventory.pods
                } else if let Some((_, pods)) = self.fleet_pods.get(member) {
                    pods
                } else {
                    continue;
                };
                wanted.extend(specs_for(
                    pods_for(&target, pods, &self.selector),
                    Some(member),
                ));
            }
        }

        // Cap concurrent streams: a DaemonSet on a 500-node cluster would
        // otherwise open 500 watches against the API server at once.
        let cap = self.config.logs.max_streams.max(1);
        let total = wanted.len();
        let truncated = total > cap;
        if truncated {
            wanted.truncate(cap);
        }

        // Drop streams whose pod is gone, keep the ones still wanted, open the
        // rest. Keeping them matters: re-attaching would replay the whole
        // history into the buffer on every inventory tick.
        let keep: HashSet<Arc<str>> = wanted.iter().map(|s| s.source()).collect();
        let stale: Vec<Arc<str>> = self
            .streams
            .keys()
            .filter(|s| !keep.contains(*s))
            .cloned()
            .collect();
        let dropped = stale.len();
        for source in stale {
            if let Some((_, handle)) = self.streams.remove(&source) {
                handle.abort();
            }
            self.attached.retain(|s| *s != source);
        }

        let had = self.attached.len();
        let mut opened = 0usize;
        for spec in wanted {
            if self.streams.contains_key(&spec.source()) {
                continue;
            }
            if self.open_stream(spec) {
                opened += 1;
            }
        }

        if truncated {
            self.set_status(
                format!("streaming {cap} of {total} containers — raise logs.max_streams for more"),
                StatusKind::Warn,
            );
        } else if fleet.is_some() && (opened > 0 || dropped > 0) {
            let (pods, clusters) = self.log_spread();
            self.set_status(
                format!(
                    "merging {pods} pods from {clusters} of {} clusters",
                    self.log_members.len()
                ),
                StatusKind::Info,
            );
        } else if had > 0 && (opened > 0 || dropped > 0) {
            // Only worth saying when this was a hand-over rather than the
            // first attach, i.e. a rollout replacing pods under a live stream.
            self.set_status(
                format!("membership changed: +{opened} -{dropped} containers"),
                StatusKind::Info,
            );
        } else if self.attached.is_empty() && self.fleet_pods_request.is_some() {
            self.set_status(
                "asking the other clusters for their pods…",
                StatusKind::Info,
            );
        } else if self.attached.is_empty() && !self.inventory_ready {
            // Right after connecting the pod list is still on its way; the
            // inventory update re-runs this and attaches.
            self.set_status("waiting for the pod inventory…", StatusKind::Info);
        } else if self.attached.is_empty() && fleet.is_none() {
            let hint = match self.current_type() {
                Some(t) if !self.selector.is_empty() => {
                    format!("no {} pods match the selector", t.name())
                }
                Some(t) => format!("no pods behind this {}", t.api.kind.to_ascii_lowercase()),
                None => "waiting for the pod inventory…".to_string(),
            };
            self.set_status(hint, StatusKind::Warn);
        }
        self.dirty = true;
    }

    /// The object to stream across the fleet, and the members it lives on:
    /// every row of the listing with the same namespace and name. `None`
    /// outside a fleet, and for Nodes, which have a journal instead.
    fn fleet_log_target(&self) -> Option<(LogTarget, Vec<String>)> {
        self.fleet.as_ref()?;
        let kind = self.current_type()?.api.kind.clone();
        if kind.eq_ignore_ascii_case("node") {
            return None;
        }
        let row = self.selected_row()?;
        let mut members: Vec<String> = Vec::new();
        for other in &self.rows {
            if other.namespace == row.namespace
                && other.name == row.name
                && !members.contains(&other.context)
            {
                members.push(other.context.clone());
            }
        }
        let target = LogTarget {
            kind,
            namespace: row.namespace.clone(),
            name: row.name.clone(),
            selector: row.selector.clone(),
        };
        Some((target, members))
    }

    /// Distinct pods and clusters behind the open streams.
    pub fn log_spread(&self) -> (usize, usize) {
        let mut pods: HashSet<&str> = HashSet::new();
        let mut clusters: HashSet<&str> = HashSet::new();
        for source in &self.attached {
            // `context|ns/pod:container`: the pod is everything before the
            // container, the cluster everything before the `|`.
            pods.insert(source.rsplit_once(':').map_or(&**source, |(pod, _)| pod));
            clusters.insert(source.split_once('|').map_or("", |(context, _)| context));
        }
        (pods.len(), clusters.len())
    }

    /// Spawn one stream, on the cluster its spec names. False when that
    /// member's client is not known yet.
    fn open_stream(&mut self, spec: StreamSpec) -> bool {
        let client = match &spec.context {
            Some(context) if *context != self.context_name => match self.fleet_pods.get(context) {
                Some((client, _)) => client.clone(),
                None => return false,
            },
            _ => self.client.clone(),
        };
        let source = spec.source();
        let handle = crate::k8s::logs::spawn(client, spec.clone(), self.log_tx.clone());
        self.streams.insert(source.clone(), (spec, handle));
        self.attached.push(source);
        true
    }

    /// The pending request for the other members' pods, for the main loop.
    pub fn take_fleet_pods_request(&mut self) -> Option<FleetPodsRequest> {
        self.fleet_pods_request.take()
    }

    /// Ask again for the other members' pods, so a rollout there hands the
    /// stream over to the new pods. Called on the fleet refresh interval.
    pub fn refresh_fleet_pods(&mut self) {
        if self.right != RightMode::Detail || self.view != View::Logs {
            return;
        }
        let Some((target, members)) = self.fleet_log_target() else {
            return;
        };
        let Some(key) = self.attached_to.clone() else {
            return;
        };
        let others: Vec<String> = members
            .into_iter()
            .filter(|m| *m != self.context_name)
            .collect();
        if !others.is_empty() {
            self.fleet_pods_request = Some(FleetPodsRequest {
                key,
                members: others,
                namespace: pods_namespace(&target),
            });
        }
    }

    /// The other members' pods arrived. An answer for logs that are no
    /// longer open is dropped.
    pub fn on_fleet_pods(&mut self, key: String, answers: crate::k8s::fleet::MemberPods) {
        if self.attached_to.as_deref() != Some(key.as_str()) {
            return;
        }
        let mut failed: Vec<String> = Vec::new();
        for (context, answer) in answers {
            match answer {
                Ok(found) => {
                    self.fleet_pods.insert(context, found);
                }
                Err(err) => {
                    tracing::warn!("pods on {context}: {err}");
                    failed.push(context);
                }
            }
        }
        self.sync_logs();
        if !failed.is_empty() {
            self.set_status(
                format!("no logs from {}: not reachable", failed.join(", ")),
                StatusKind::Warn,
            );
        }
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
        for (_, (_, handle)) in self.streams.drain() {
            handle.abort();
        }
        self.attached.clear();
        self.attached_to = None;
    }

    /// Reopen every current stream with the current toggles (timestamps,
    /// previous), on the cluster each one came from.
    fn restream(&mut self) {
        let mut specs: Vec<StreamSpec> = self
            .attached
            .iter()
            .filter_map(|source| self.streams.get(source).map(|(spec, _)| spec.clone()))
            .collect();
        for (_, (_, handle)) in self.streams.drain() {
            handle.abort();
        }
        self.attached.clear();
        // The new streams replay the history with the new settings.
        self.buffer.clear();
        self.scroll = 0;
        self.search.current = None;
        for spec in &mut specs {
            spec.tail = self.tail_lines();
            spec.since_seconds = self.config.logs.since_seconds;
            spec.timestamps = self.timestamps;
            spec.previous = self.previous;
        }
        for spec in specs {
            self.open_stream(spec);
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
        // A full pass, but only on user action (new query, filter change);
        // arriving lines keep the count up to date in `on_log`.
        self.search.total = self.buffer.count_matches(re);
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
            InputMode::Search
                | InputMode::Filter
                | InputMode::Namespace
                | InputMode::RowFilter
                | InputMode::Selector
                | InputMode::Fleet
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
            KeyCode::Char('l') => self.set_view(View::Logs),
            KeyCode::Char('m') => self.set_view(View::Metrics),
            // `e` is events, like `l` logs and `m` metrics; `E` still works
            // for anyone used to the old binding.
            KeyCode::Char('e') | KeyCode::Char('E') => self.set_view(View::Events),
            KeyCode::Char('a') if self.fleet.is_none() => self.set_status(
                "a compares an object across a fleet: start with -u stg, or / on the clusters pane",
                StatusKind::Info,
            ),
            KeyCode::Char('a') => {
                // Pressing it again re-reads every member.
                self.compare_for = None;
                self.set_view(View::Compare);
            }
            KeyCode::Char('d') => self.toggle_describe(),

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
            KeyCode::Char('/') => self.enter_prompt(match (self.pane, self.right) {
                (Pane::Contexts, _) => InputMode::Fleet,
                (_, RightMode::Browse) => InputMode::RowFilter,
                (_, RightMode::Detail) => InputMode::Search,
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
            // `L` is the label selector while browsing, and the log level
            // threshold once a log view is open — the two never overlap.
            KeyCode::Char('L') if self.right == RightMode::Browse => {
                self.mode = InputMode::Selector;
                self.input = self.selector_text.clone();
                self.dirty = true;
            }
            KeyCode::Char('L') => {
                let mut filter = self.buffer.filter.clone();
                filter.min_level = Level::next_threshold(filter.min_level);
                let label = filter.min_level.label();
                let everything = filter.min_level == Level::Trace;
                self.buffer.set_filter(filter);
                self.scroll_to_bottom();
                // `L` up to ERROR is what "errors only" used to be.
                self.set_status(
                    if everything {
                        "showing every level — L to narrow".to_string()
                    } else {
                        format!("showing {label} and above — L for the next level")
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

            // Triage: narrow any listing to just the objects in trouble.
            KeyCode::Char('!') => {
                self.problems_only = !self.problems_only;
                self.rebuild_row_view();
                self.set_status(
                    if self.problems_only {
                        format!(
                            "showing only {} with problems: {} of {} — ! to show all",
                            self.resource_label(),
                            self.row_view.len(),
                            self.rows.len()
                        )
                    } else {
                        format!("showing all {} {}", self.rows.len(), self.resource_label())
                    },
                    StatusKind::Info,
                );
            }

            KeyCode::Char('v') if self.describe_open => self.toggle_secret_values(),

            // Cycle kubelet / containerd / kernel on a Node's log tab.
            KeyCode::Char('v')
                if self.view == View::Logs
                    && self
                        .log_target()
                        .is_some_and(|t| t.kind.eq_ignore_ascii_case("node")) =>
            {
                self.node_service = self.node_service.wrapping_add(1);
                self.node_logs_for = None;
                self.sync_logs();
                let service = self.node_service();
                self.set_status(format!("node service: {service}"), StatusKind::Info);
            }

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

            _ => {}
        }
    }

    /// `Esc` steps back out: any tab, describe included → browse → quit.
    fn on_escape(&mut self) {
        if self.describe_open {
            self.describe_open = false;
            self.right = RightMode::Browse;
            self.dirty = true;
            return;
        }
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
                    InputMode::Selector => self.apply_selector(&value),
                    InputMode::Fleet => self.apply_fleet_input(&value),
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
        if self.open_remote(Some(view), false) {
            return;
        }
        // Picking another tab leaves describe, as it leaves any tab.
        self.describe_open = false;
        self.view = view;
        // Picking a tab is also how you open the detail pane.
        if self.right == RightMode::Browse && self.selected_row().is_some() {
            self.right = RightMode::Detail;
        }
        // Unconditional: arriving on the logs tab from *any* other tab has to
        // attach, not just the Browse → Detail transition.
        self.sync_logs();
        self.sync_describe();
        self.sync_compare();
        self.dirty = true;
    }

    /// Fetch the open object from every fleet member, unless that is what
    /// the compare tab already shows.
    fn sync_compare(&mut self) {
        if self.right != RightMode::Detail || self.view != View::Compare || self.describe_open {
            return;
        }
        let (Some(fleet), Some(kind), Some(row)) =
            (&self.fleet, self.current_type(), self.selected_row())
        else {
            return;
        };
        let key = format!(
            "{}|{}|{}/{}",
            fleet.name,
            kind.qualified(),
            row.namespace,
            row.name
        );
        if self.compare_for.as_deref() == Some(key.as_str()) {
            return;
        }
        self.compare_for = Some(key);
        self.compare.clear();
        self.compare_wanted = true;
        self.compare_loading = true;
        self.dirty = true;
    }

    /// The pending compare fetch: its key, the kind, namespace, name and the
    /// members to ask.
    pub fn take_compare_request(
        &mut self,
    ) -> Option<(String, ResourceType, String, String, Vec<String>)> {
        if !std::mem::take(&mut self.compare_wanted) {
            return None;
        }
        let key = self.compare_for.clone()?;
        let members = self.fleet.as_ref()?.members.clone();
        let kind = self.current_type()?.clone();
        let row = self.selected_row()?;
        Some((key, kind, row.namespace.clone(), row.name.clone(), members))
    }

    pub fn on_compare(
        &mut self,
        key: String,
        answers: Vec<(String, Result<serde_json::Value, String>)>,
    ) {
        if self.compare_for.as_deref() != Some(key.as_str()) {
            return;
        }
        self.compare = answers
            .into_iter()
            .map(|(context, answer)| crate::compare::Member::new(context, answer))
            .collect();
        self.compare_loading = false;
        let drift = crate::compare::drift(&self.compare);
        self.set_status(
            if drift.is_empty() {
                crate::compare::agreement(&self.compare)
            } else {
                let mut differ: Vec<&String> = drift
                    .images
                    .iter()
                    .chain(&drift.replicas)
                    .chain(&drift.spec)
                    .collect();
                differ.sort();
                differ.dedup();
                format!(
                    "differs on {}",
                    differ
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            if drift.is_empty() {
                StatusKind::Info
            } else {
                StatusKind::Warn
            },
        );
        self.dirty = true;
    }

    /// Set the selector from the command line, before anything is listed.
    pub fn set_selector(&mut self, raw: &str) {
        match LabelSelector::parse(raw) {
            Ok(selector) => {
                self.selector = selector;
                self.selector_text = raw.trim().to_string();
            }
            Err(err) => self.set_status(format!("bad --selector: {err}"), StatusKind::Error),
        }
    }

    /// Apply a `-l` selector. It narrows the browsed list server-side on the
    /// next refresh, and narrows which pods a workload's logs come from now.
    fn apply_selector(&mut self, raw: &str) {
        match LabelSelector::parse(raw) {
            Ok(selector) => {
                self.selector = selector;
                self.selector_text = raw.trim().to_string();
                self.request_rows();
                // The stream set depends on the selector, so re-derive it.
                self.attached_to = None;
                self.sync_logs();
                self.set_status(
                    if self.selector_text.is_empty() {
                        "selector cleared".to_string()
                    } else {
                        format!("selector: {}", self.selector_text)
                    },
                    StatusKind::Info,
                );
            }
            Err(err) => self.set_status(format!("bad selector: {err}"), StatusKind::Error),
        }
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
        if self.describe_open {
            let max = self
                .describe_len()
                .saturating_sub(self.viewport_height.max(1));
            self.describe_scroll =
                (self.describe_scroll as isize + delta).clamp(0, max as isize) as usize;
            self.dirty = true;
            return;
        }
        match (self.pane, self.right) {
            (Pane::Contexts, _) => {
                let len = self.pane_len();
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
                self.sync_describe();
                self.dirty = true;
            }
            (Pane::Resources, RightMode::Detail) => match self.view {
                View::Logs => self.scroll_by(delta),
                View::Compare => {}
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
                self.describe_scroll = 0;
                self.follow = false;
                self.scroll = 0;
            }
        }
        self.dirty = true;
    }

    fn goto_end(&mut self) {
        match (self.pane, self.right) {
            (Pane::Contexts, _) => {
                self.context_selected = self.pane_len().saturating_sub(1);
            }
            (Pane::Resources, RightMode::Browse) => {
                self.row_selected = self.row_view.len().saturating_sub(1);
                self.rebuild_event_view();
            }
            (Pane::Resources, RightMode::Detail) => {
                self.describe_scroll = self
                    .describe_len()
                    .saturating_sub(self.viewport_height.max(1));
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
                    if self.open_remote(Some(View::Logs), false) {
                        return;
                    }
                    // Enter is the same as `l`: the common case is logs.
                    self.view = View::Logs;
                    self.right = RightMode::Detail;
                    self.sync_logs();
                    self.dirty = true;
                }
            }
        }
    }

    /// `s`: write the visible buffer to `kscope-YYYYMMDD-HHMMSS.log` in
    /// `logs.save_dir` (the platform data directory by default, never the
    /// working directory), creating it if needed. The status bar names the
    /// full path, since it is not where kscope was started.
    fn save_buffer(&mut self) {
        let Some(dir) = self.config.logs.save_dir() else {
            self.set_status(
                "save failed: no data directory found; set logs.save_dir in the config",
                StatusKind::Error,
            );
            return;
        };
        let path = dir.join(format!(
            "kscope-{}.log",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, self.buffer.to_plain_text()));
        match written {
            Ok(()) => self.set_status(
                format!(
                    "wrote {} lines to {}",
                    self.buffer.view_len(),
                    path.display()
                ),
                StatusKind::Info,
            ),
            Err(err) => self.set_status(
                format!("save failed: {}: {err}", path.display()),
                StatusKind::Error,
            ),
        }
    }

    /// Elapsed since the last metrics poll, for the status bar.
    pub fn metrics_age(&self) -> Option<Duration> {
        self.metrics.last_update.map(|t| t.elapsed())
    }
}

/// Byte ranges of the lines in `text`, as `str::lines` would split it.
fn line_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            spans.push(start..i);
            start = i + 1;
        }
    }
    if start < text.len() {
        spans.push(start..text.len());
    }
    spans
}

/// The namespace whose pods a log target draws from.
fn pods_namespace(target: &LogTarget) -> String {
    if target.kind.eq_ignore_ascii_case("namespace") {
        target.name.clone()
    } else {
        target.namespace.clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn line_spans_split_like_str_lines() {
        for text in ["", "a", "a\nb", "a\nb\n", "a\r\n\nc", "\n\n"] {
            let spans: Vec<&str> = line_spans(text)
                .into_iter()
                .map(|r| text[r].trim_end_matches('\r'))
                .collect();
            assert_eq!(spans, text.lines().collect::<Vec<_>>(), "{text:?}");
        }
    }

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

    // ---------------------------------------------------------------------
    // App-level behaviour, against a client that is never actually called.

    fn offline_client() -> kube::Client {
        let config = kube::Config::new("http://127.0.0.1:9".parse().unwrap());
        kube::Client::try_from(config).unwrap()
    }

    fn pods_type() -> ResourceType {
        ResourceType {
            api: kube::core::ApiResource {
                group: String::new(),
                version: "v1".into(),
                api_version: "v1".into(),
                kind: "Pod".into(),
                plural: "pods".into(),
            },
            namespaced: true,
        }
    }

    fn row(context: &str, name: &str) -> ResourceRow {
        ResourceRow {
            context: context.into(),
            namespace: "shop".into(),
            name: name.into(),
            status: "Running".into(),
            ..Default::default()
        }
    }

    pub(crate) fn test_app(config: Config) -> App {
        let (log_tx, _) = tokio::sync::mpsc::channel(16);
        let contexts = ["stg-eu", "stg-us", "prod-eu"]
            .iter()
            .map(|name| ContextEntry {
                name: name.to_string(),
                cluster: name.to_string(),
            })
            .collect();
        let mut app = App::new(
            config,
            offline_client(),
            Scope::Namespace("default".into()),
            log_tx,
            "stg-eu".into(),
            "me".into(),
            "v1.36.1".into(),
            contexts,
        );
        app.on_resource_types(vec![pods_type()]);
        app
    }

    fn batch(lines: std::ops::Range<usize>) -> LogEvent {
        LogEvent::Batch {
            source: Arc::from("shop/api:app"),
            lines: lines.map(|i| format!("INFO line {i:04}")).collect(),
        }
    }

    #[tokio::test]
    async fn saving_writes_into_the_configured_dir_and_names_the_full_path() {
        let dir = std::env::temp_dir().join(format!("kscope-save-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = Config::default();
        // Two levels that do not exist yet: both are created.
        config.logs.save_dir = Some(dir.join("nested"));
        let mut app = test_app(config);
        app.on_log(batch(0..3));

        app.save_buffer();
        assert_eq!(app.status.kind, StatusKind::Info, "{}", app.status.text);
        let saved: Vec<_> = std::fs::read_dir(dir.join("nested"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(saved.len(), 1);
        let file = &saved[0];
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("kscope-") && name.ends_with(".log"),
            "{name}"
        );
        assert!(
            app.status.text.ends_with(&file.display().to_string()),
            "the status names the full path: {}",
            app.status.text
        );
        assert_eq!(
            std::fs::read_to_string(file).unwrap().lines().count(),
            3,
            "the visible lines"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn eviction_keeps_the_search_cursor_and_scroll_on_their_lines() {
        let mut config = Config::default();
        config.logs.buffer_lines = 64; // the smallest ring
        let mut app = test_app(config);
        app.follow = false;
        app.on_log(batch(0..64));

        // Park the cursor on "line 0040" and the scroll on "line 0030".
        app.search.query = "line 0040".into();
        app.search.regex = app.compile("line 0040");
        app.search.current = Some(40);
        app.scroll = 30;

        // Ten more lines evict ten from the front.
        app.on_log(batch(64..74));
        let at = |i: usize| app.buffer.view_line(i).map(|l| l.raw.to_string());
        assert_eq!(
            at(app.search.current.unwrap()).as_deref(),
            Some("INFO line 0040")
        );
        assert_eq!(at(app.scroll).as_deref(), Some("INFO line 0030"));

        // Once the matched line itself is evicted there is no current match.
        app.on_log(batch(74..120));
        assert_eq!(app.search.current, None);
    }

    #[tokio::test]
    async fn describe_is_a_tab_you_can_leave_for_logs() {
        let mut app = test_app(Config::default());
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-eu", "api-1")],
            failed: vec![],
        }));
        let key = |app: &mut App, c: char| {
            app.on_event(Event::Key(KeyEvent::from(KeyCode::Char(c))));
        };

        // `d` from the list opens the detail pane on the describe tab.
        key(&mut app, 'd');
        assert!(app.describe_open);
        assert_eq!(app.right, RightMode::Detail);

        // `l` leads straight on to the logs, as from any other tab.
        key(&mut app, 'l');
        assert!(!app.describe_open);
        assert_eq!((app.right, app.view), (RightMode::Detail, View::Logs));

        // And back: `d` again, then `m` for metrics.
        key(&mut app, 'd');
        assert!(app.describe_open);
        key(&mut app, 'm');
        assert!(!app.describe_open);
        assert_eq!(app.view, View::Metrics);

        // Esc leaves describe for the list, like every tab.
        key(&mut app, 'd');
        app.on_event(Event::Key(KeyEvent::from(KeyCode::Esc)));
        assert!(!app.describe_open);
        assert_eq!(app.right, RightMode::Browse);
        assert!(!app.should_quit);
    }

    #[tokio::test]
    async fn opening_a_row_on_another_member_switches_there_first() {
        let mut app = test_app(Config::default());
        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        assert_eq!(app.scope, Scope::AllNamespaces);
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-eu", "api-1"), row("stg-us", "api-2")],
            failed: vec![],
        }));
        // Filtering reaches the context column too.
        app.row_filter = Some("stg-us".into());
        app.rebuild_row_view();
        assert_eq!(app.row_view.len(), 1);

        // Metrics of the stg-us row: nothing is fetched from the wrong
        // cluster, a switch is requested instead.
        app.set_view(View::Metrics);
        assert_eq!(app.pending_context.as_deref(), Some("stg-us"));
        assert_eq!(app.right, RightMode::Browse);
        assert!(app.log_target().is_none());
        assert!(app.take_describe_request().is_none());

        // The switch lands: the fleet list survives and the row opens.
        let context = app.pending_context.take().unwrap();
        let scope = app.scope.clone();
        app.adopt_context(offline_client(), scope, context, "me".into(), "v".into());
        assert_eq!(app.right, RightMode::Detail);
        assert_eq!(app.view, View::Metrics);
        assert_eq!(app.selected_row().unwrap().identity(), "stg-us:shop/api-2");
        assert_eq!(app.rows.len(), 2);
        assert!(app.log_target().is_some());
    }

    fn pod(name: &str) -> PodInfo {
        PodInfo {
            namespace: "shop".into(),
            name: name.into(),
            phase: "Running".into(),
            containers: vec![crate::k8s::ContainerInfo {
                name: "app".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn fleet_logs_merge_every_member_without_switching() {
        let mut app = test_app(Config::default());
        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        // `api` runs on both members; `cart` only here.
        app.on_listing(Ok(Listing {
            rows: vec![
                row("stg-eu", "api"),
                row("stg-eu", "cart"),
                row("stg-us", "api"),
            ],
            failed: vec![],
        }));
        app.inventory.pods = vec![pod("api")];
        app.inventory_ready = true;
        // The cursor is on the stg-us row: Enter still stays put.
        app.row_selected = 2;
        assert_eq!(app.selected_row().unwrap().context, "stg-us");
        app.activate_selection();
        assert!(app.pending_context.is_none(), "logs need no switch");
        assert_eq!((app.right, app.view), (RightMode::Detail, View::Logs));
        assert_eq!(app.log_members, vec!["stg-eu", "stg-us"]);

        // This member streams at once, labelled with its cluster.
        assert_eq!(app.attached, vec![Arc::from("stg-eu|shop/api:app")]);

        // The other member's pods are asked for, in the object's namespace.
        let request = app.take_fleet_pods_request().unwrap();
        assert_eq!(request.members, vec!["stg-us"]);
        assert_eq!(request.namespace, "shop");

        // They arrive: both clusters' replicas are merged.
        app.on_fleet_pods(
            request.key,
            vec![(
                "stg-us".into(),
                Ok((offline_client(), vec![pod("api"), pod("other")])),
            )],
        );
        assert_eq!(app.attached.len(), 2);
        assert!(app.attached.contains(&Arc::from("stg-us|shop/api:app")));
        assert_eq!(app.log_spread(), (2, 2));
        assert!(
            app.status.text.contains("2 pods from 2 of 2 clusters"),
            "{}",
            app.status.text
        );

        // An answer for logs no longer open is dropped.
        app.on_fleet_pods(
            "old".into(),
            vec![("stg-us".into(), Ok((offline_client(), vec![])))],
        );
        assert_eq!(app.attached.len(), 2);
    }

    #[tokio::test]
    async fn compare_reads_every_member_and_names_the_odd_one() {
        let mut app = test_app(Config::default());
        // Outside a fleet there is nothing to compare with.
        app.on_event(Event::Key(KeyEvent::from(KeyCode::Char('a'))));
        assert_ne!(app.view, View::Compare);

        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into(), "stg-ap".into()],
        });
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-eu", "api"), row("stg-us", "api")],
            failed: vec![],
        }));
        app.row_selected = 1;
        app.on_event(Event::Key(KeyEvent::from(KeyCode::Char('a'))));
        assert!(app.pending_context.is_none(), "compare needs no switch");
        assert_eq!((app.right, app.view), (RightMode::Detail, View::Compare));
        let (key, _, namespace, name, members) = app.take_compare_request().unwrap();
        assert_eq!((namespace.as_str(), name.as_str()), ("shop", "api"));
        assert_eq!(members, vec!["stg-eu", "stg-us", "stg-ap"]);
        assert!(app.take_compare_request().is_none(), "asked once");

        let pod = |image: &str| serde_json::json!({"kind": "Pod", "spec": {"containers": [{"image": image}]}});
        app.on_compare(
            key,
            vec![
                ("stg-eu".into(), Ok(pod("api:2"))),
                ("stg-us".into(), Ok(pod("api:1"))),
                ("stg-ap".into(), Err("pods \"api\" not found (404)".into())),
            ],
        );
        assert_eq!(app.compare.len(), 3);
        assert_eq!(app.compare[2].state, crate::compare::State::Absent);
        // A tie goes to the first member, so stg-us is the odd one out.
        assert_eq!(app.status.text, "differs on stg-us");
    }

    #[tokio::test]
    async fn v_reveals_a_secret_decoded_and_resets_for_the_next_object() {
        let mut app = test_app(Config::default());
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-eu", "db"), row("stg-eu", "other")],
            failed: vec![],
        }));
        let v = |app: &mut App| app.on_event(Event::Key(KeyEvent::from(KeyCode::Char('v'))));
        app.on_event(Event::Key(KeyEvent::from(KeyCode::Char('d'))));
        app.on_describe(Ok(serde_json::json!({
            "apiVersion": "v1", "kind": "Secret", "data": {"password": "aHVudGVyMg=="}
        })));
        assert!(app.describe_text.contains("<redacted, 7 bytes>"));
        v(&mut app);
        assert!(
            app.describe_text.contains("password: hunter2"),
            "{}",
            app.describe_text
        );
        v(&mut app);
        assert!(!app.describe_text.contains("hunter2"));

        // Revealed stays with that Secret only: describing the next row
        // starts hidden again.
        v(&mut app);
        assert!(app.secrets_revealed);
        app.on_event(Event::Key(KeyEvent::from(KeyCode::Esc)));
        app.move_cursor(1);
        app.on_event(Event::Key(KeyEvent::from(KeyCode::Char('d'))));
        assert_eq!(app.selected_row().unwrap().name, "other");
        assert!(!app.secrets_revealed);

        // And it can be switched off for a whole team.
        let mut config = Config::default();
        config.describe.allow_reveal = false;
        let mut app = test_app(config);
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-eu", "db")],
            failed: vec![],
        }));
        app.on_event(Event::Key(KeyEvent::from(KeyCode::Char('d'))));
        app.on_describe(Ok(serde_json::json!({
            "apiVersion": "v1", "kind": "Secret", "data": {"password": "aHVudGVyMg=="}
        })));
        v(&mut app);
        assert!(!app.describe_text.contains("hunter2"));
        assert!(app.status.text.contains("allow_reveal"));
    }

    #[tokio::test]
    async fn a_fleet_reports_members_that_did_not_answer() {
        let mut app = test_app(Config::default());
        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-eu", "api-1")],
            failed: vec![("stg-us".into(), "no answer in 10s".into())],
        }));
        assert_eq!(app.rows.len(), 1);
        assert!(app.rows_error.is_none());
        assert!(app.status.text.contains("stg-us unreachable"));

        // Nobody answering at all is an error, not an empty list.
        app.on_listing(Ok(Listing {
            rows: vec![],
            failed: vec![("stg-eu".into(), "x".into()), ("stg-us".into(), "y".into())],
        }));
        assert!(app
            .rows_error
            .as_deref()
            .unwrap()
            .contains("no fleet member answered"));
    }

    #[tokio::test]
    async fn a_fleet_from_the_command_line_keeps_its_namespace() {
        let mut app = test_app(Config::default());
        app.scope = Scope::Namespace("shop".into());
        app.open_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        assert_eq!(app.scope, Scope::Namespace("shop".into()));
        assert!(!app.pollers_stale);
        // Picked in the TUI, a fleet spans every namespace.
        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        assert_eq!(app.scope, Scope::AllNamespaces);
    }

    #[tokio::test]
    async fn after_a_switch_the_pane_cursor_and_status_tell_the_truth() {
        let mut app = test_app(Config::default());
        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        app.on_listing(Ok(Listing {
            rows: vec![row("stg-us", "api-2")],
            failed: vec![],
        }));
        app.set_view(View::Metrics);
        let context = app.pending_context.take().unwrap();
        let scope = app.scope.clone();
        app.adopt_context(offline_client(), scope, context, "me".into(), "v".into());
        // stg-us is the second context; no configured fleets precede it.
        assert_eq!(app.context_selected, 1);
        // The pod inventory has not arrived yet: that is not "no pods".
        app.set_view(View::Logs);
        assert_eq!(app.status.text, "waiting for the pod inventory…");
    }

    #[tokio::test]
    async fn picking_a_single_context_leaves_the_fleet() {
        let mut app = test_app(Config::default());
        app.enter_fleet(Fleet {
            name: "stg".into(),
            members: vec!["stg-eu".into(), "stg-us".into()],
        });
        // The pane lists configured fleets first; none here, so index 2 is
        // the third context.
        app.context_selected = 2;
        app.switch_context();
        assert!(app.fleet.is_none());
        assert_eq!(app.pending_context.as_deref(), Some("prod-eu"));
    }
}
