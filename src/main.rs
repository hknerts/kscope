//! kscope — a read-only Kubernetes TUI for logs and live metrics.

mod cli;

use kscope::{app, config, event, k8s, ui};

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::cli::Cli;
use app::{App, ContextEntry};
use config::Config;
use k8s::discovery::Scope;
use k8s::fleet::{Fleet, Listing};
use k8s::resources::{ResourceRow, ResourceType};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(&cli);

    let mut config = Config::load(cli.config.as_deref())?;
    if let Some(tail) = cli.tail {
        config.logs.tail_lines = tail;
    }
    if let Some(buffer) = cli.buffer {
        config.logs.buffer_lines = buffer;
    }
    if let Some(refresh) = cli.refresh {
        config.metrics.refresh_ms = refresh;
    }
    if let Some(since) = cli.since {
        config.logs.since_seconds = Some(since);
    }
    if cli.timestamps {
        config.logs.timestamps = true;
    }

    // A fleet decides which context to connect to first: `--context` if it
    // is given, else the kubeconfig's current context when that is a member,
    // else the first member.
    let kube_contexts = k8s::resources::contexts();
    let names: Vec<String> = kube_contexts.keys().cloned().collect();
    let fleet = match &cli.fleet {
        Some(spec) => Some(
            Fleet::resolve(spec, &config.fleets, &names)
                .with_context(|| format!("no kubeconfig context matches fleet {spec:?}"))?,
        ),
        None => None,
    };
    let fleet_clients = k8s::fleet::Clients::default();
    let initial_context = match (&cli.context, &fleet) {
        (Some(context), _) => Some(context.clone()),
        (None, Some(fleet)) => {
            let current = kube::config::Kubeconfig::read()
                .ok()
                .and_then(|k| k.current_context);
            Some(
                k8s::fleet::first_reachable(&fleet_clients, &fleet.members, current.as_deref())
                    .await
                    .with_context(|| format!("no member of fleet {} answered", fleet.name))?,
            )
        }
        (None, None) => None,
    };

    let (client, default_ns, context_name, user_name) = k8s::connect(initial_context.as_deref())
        .await
        .context("could not reach the cluster")?;

    // Searching a fleet is about finding things wherever they are, so it
    // spans every namespace unless one is asked for.
    let explicit_ns = cli
        .namespace
        .clone()
        .or_else(|| config.general.namespace.clone());
    let scope = if cli.all_namespaces
        || (explicit_ns.is_none() && (fleet.is_some() || cli.find.is_some()))
    {
        Scope::AllNamespaces
    } else {
        Scope::Namespace(explicit_ns.unwrap_or(default_ns))
    };

    // Non-interactive mode: search and exit.
    if let Some(needle) = &cli.find {
        fleet_clients.insert(&context_name, client.clone()).await;
        let found = find(
            &fleet_clients,
            &context_name,
            fleet.as_ref(),
            cli.kind.as_deref().unwrap_or("pods"),
            &scope,
            cli.selector.as_deref().unwrap_or(""),
            needle,
        )
        .await?;
        std::process::exit(if found { 0 } else { 1 });
    }

    // Non-interactive mode: dump and exit.
    if let Some((pod, container)) = cli.dump_target() {
        let spec = k8s::logs::StreamSpec {
            namespace: match &scope {
                Scope::Namespace(ns) => ns.clone(),
                Scope::AllNamespaces => "default".to_string(),
            },
            pod,
            container,
            tail: if config.logs.tail_lines <= 0 {
                None
            } else {
                Some(config.logs.tail_lines)
            },
            since_seconds: config.logs.since_seconds,
            timestamps: config.logs.timestamps,
            previous: false,
        };
        print!("{}", k8s::logs::snapshot(client, &spec).await?);
        return Ok(());
    }

    let mut terminal = setup_terminal(config.general.mouse)?;
    let result = run(
        &mut terminal,
        config,
        client,
        scope,
        context_name,
        user_name,
        Start {
            clients: fleet_clients,
            selector: cli.selector.clone().unwrap_or_default(),
            kind: cli.kind.clone(),
            fleet,
            contexts: kube_contexts,
        },
    )
    .await;
    restore_terminal(&mut terminal)?;
    result
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// What the command line asked the TUI to start on.
struct Start {
    /// Clients already built while picking the first fleet member.
    clients: k8s::fleet::Clients,
    selector: String,
    kind: Option<String>,
    fleet: Option<Fleet>,
    contexts: std::collections::BTreeMap<String, String>,
}

/// `--find`: list `kind` on every member (or just the connected context),
/// print the rows whose namespace or name contains `needle`, and report
/// whether there were any. Unreachable members go to stderr, not stdout, so
/// the table stays pipeable.
async fn find(
    clients: &k8s::fleet::Clients,
    context_name: &str,
    fleet: Option<&Fleet>,
    kind: &str,
    scope: &Scope,
    selector: &str,
    needle: &str,
) -> Result<bool> {
    let client = clients.get(context_name).await?;
    let types = k8s::resources::discover(&client)
        .await
        .context("discovering resource types")?;
    let kind = k8s::resources::resolve(&types, kind)
        .with_context(|| format!("no such resource type: {kind}"))?
        .clone();
    let members = match fleet {
        Some(fleet) => fleet.members.clone(),
        None => vec![context_name.to_string()],
    };
    let listing = k8s::fleet::list(clients, &members, &kind, scope, selector).await;
    for (context, err) in &listing.failed {
        eprintln!("kscope: {context}: {err}");
    }
    if listing.failed.len() == members.len() {
        anyhow::bail!("no context answered");
    }

    let needle = needle.to_ascii_lowercase();
    let hits: Vec<&ResourceRow> = listing
        .rows
        .iter()
        .filter(|r| {
            r.name.to_ascii_lowercase().contains(&needle)
                || r.namespace.to_ascii_lowercase().contains(&needle)
        })
        .collect();
    let width = |f: fn(&ResourceRow) -> &str, title: &str| {
        hits.iter()
            .map(|r| f(r).chars().count())
            .chain([title.len()])
            .max()
            .unwrap_or(0)
    };
    let (wc, wn, wm, ws) = (
        width(|r| &r.context, "CONTEXT"),
        width(|r| &r.namespace, "NAMESPACE"),
        width(|r| &r.name, "NAME"),
        width(|r| &r.status, "STATUS"),
    );
    if !hits.is_empty() {
        println!(
            "{:wc$}  {:wn$}  {:wm$}  {:ws$}  AGE",
            "CONTEXT", "NAMESPACE", "NAME", "STATUS"
        );
    }
    for r in &hits {
        println!(
            "{:wc$}  {:wn$}  {:wm$}  {:ws$}  {}",
            r.context,
            r.namespace,
            r.name,
            r.status,
            k8s::fmt_age(r.age_seconds)
        );
    }
    Ok(!hits.is_empty())
}

fn setup_terminal(mouse: bool) -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    if mouse {
        execute!(stdout, EnableMouseCapture)?;
    }
    install_panic_hook();
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Make sure a panic never leaves the user with a broken terminal.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original(info);
    }));
}

fn init_tracing(cli: &Cli) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("KSCOPE_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    match &cli.log_file {
        Some(path) => {
            if let Ok(file) = std::fs::File::create(path) {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(file)
                    .with_ansi(false)
                    .try_init();
            }
        }
        None => {
            // The TUI owns the terminal, so without an explicit file we stay
            // quiet rather than corrupting the screen.
        }
    }
}

/// The three background pollers, kept together so a context switch can tear
/// them all down and bring them back up against the new cluster.
struct Pollers {
    inventory: tokio::task::JoinHandle<()>,
    metrics: tokio::task::JoinHandle<()>,
    events: tokio::task::JoinHandle<()>,
}

impl Pollers {
    fn spawn(
        client: &kube::Client,
        scope: &Scope,
        config: &Config,
        inv_tx: tokio::sync::mpsc::Sender<k8s::discovery::Update>,
        met_tx: tokio::sync::mpsc::Sender<k8s::metrics::MetricsEvent>,
        evt_tx: tokio::sync::mpsc::Sender<k8s::events::EventUpdate>,
    ) -> Self {
        Self {
            inventory: k8s::discovery::spawn(
                client.clone(),
                scope.clone(),
                Duration::from_millis(config.general.inventory_refresh_ms),
                inv_tx,
            ),
            metrics: k8s::metrics::spawn(
                client.clone(),
                scope.clone(),
                Duration::from_millis(config.metrics.refresh_ms),
                met_tx,
            ),
            events: k8s::events::spawn(
                client.clone(),
                scope.clone(),
                Duration::from_millis(config.general.events_refresh_ms),
                evt_tx,
            ),
        }
    }

    fn abort(&self) {
        self.inventory.abort();
        self.metrics.abort();
        self.events.abort();
    }
}

async fn run(
    terminal: &mut Tui,
    config: Config,
    client: kube::Client,
    scope: Scope,
    context_name: String,
    user_name: String,
    start: Start,
) -> Result<()> {
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(1024);
    let (inv_tx, mut inv_rx) = tokio::sync::mpsc::channel(16);
    let (met_tx, mut met_rx) = tokio::sync::mpsc::channel(16);
    let (evt_tx, mut evt_rx) = tokio::sync::mpsc::channel(16);
    // Resource listings are one-shot requests rather than a poller, so they
    // get their own channel and a task spawned per refresh. Each result is
    // tagged with what was asked for, so an answer that arrives after the user
    // has moved on to another kind is recognised and dropped.
    type Rows = (String, Result<Listing, String>);
    let (row_tx, mut row_rx) = tokio::sync::mpsc::channel::<Rows>(8);
    // Header details and context switches are network round trips (a context
    // switch may even run an exec credential plugin), so neither is awaited
    // on the UI loop.
    let (ver_tx, mut ver_rx) = tokio::sync::mpsc::channel::<(String, String)>(2);
    type Switched = Result<(kube::Client, String, String, String), (String, String)>;
    let (ctx_tx, mut ctx_rx) = tokio::sync::mpsc::channel::<Switched>(2);
    let (type_tx, mut type_rx) = tokio::sync::mpsc::channel::<Vec<ResourceType>>(4);
    // Node journals are a one-shot fetch rather than a stream.
    type NodeLogs = (String, &'static str, Result<String, String>);
    let (node_tx, mut node_rx) = tokio::sync::mpsc::channel::<NodeLogs>(4);
    let (desc_tx, mut desc_rx) = tokio::sync::mpsc::channel::<Result<serde_json::Value, String>>(4);
    // PVC usage lives on the kubelet, not the API server, so it is polled
    // separately and is allowed to be unavailable.
    let (vol_tx, mut vol_rx) = tokio::sync::mpsc::channel::<
        Result<std::collections::HashMap<String, k8s::volume_stats::VolumeUsage>, String>,
    >(4);

    let frame_budget = Duration::from_millis(1000 / config.general.max_fps.max(1) as u64);

    let mut pollers = Pollers::spawn(
        &client,
        &scope,
        &config,
        inv_tx.clone(),
        met_tx.clone(),
        evt_tx.clone(),
    );

    spawn_version(&client, &context_name, ver_tx.clone());
    // Fleet members get their own clients, built once and kept.
    let fleet_clients = start.clients;
    fleet_clients.insert(&context_name, client.clone()).await;
    let contexts = start
        .contexts
        .into_iter()
        .map(|(name, cluster)| ContextEntry { name, cluster })
        .collect();
    let mut input = event::spawn();
    let mut app = App::new(
        config,
        client,
        scope,
        log_tx,
        context_name,
        user_name,
        "…".to_string(),
        contexts,
    );

    if !start.selector.is_empty() {
        app.set_selector(&start.selector);
    }
    app.start_kind = start.kind;
    if let Some(fleet) = start.fleet {
        app.enter_fleet(fleet);
        // Entering a fleet asks for the pollers to be re-scoped, but these
        // were spawned against the right scope a moment ago.
        app.pollers_stale = false;
    }
    spawn_discovery(&app.client, type_tx.clone());

    // Draw once immediately so the user sees the shell before the first poll.
    terminal.draw(|f| ui::draw(f, &mut app))?;
    let mut last_draw = Instant::now();
    // Housekeeping only (list refresh, PVC usage polling). Frames are not
    // driven by a ticker: an idle kscope does not wake up 30 times a second.
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    // `loading` is edge-triggered: the app raises it, we service it once.
    // `listing` holds the request in flight, if any.
    let mut listing: Option<String> = None;
    let mut last_listed = Instant::now();
    let list_refresh = Duration::from_millis(app.config.general.inventory_refresh_ms.max(1000));

    loop {
        let next_frame = tokio::time::Instant::from_std(last_draw + frame_budget);
        tokio::select! {
            biased;

            Some(ev) = input.recv() => app.on_event(ev),
            Some(update) = inv_rx.recv() => app.on_update(update),
            Some(batch) = log_rx.recv() => app.on_log(batch),
            Some(sample) = met_rx.recv() => app.on_metrics(sample),
            Some(events) = evt_rx.recv() => app.on_events(events),
            Some(types) = type_rx.recv() => {
                app.on_resource_types(types);
                app.request_rows();
            }
            Some((key, rows)) = row_rx.recv() => {
                listing = None;
                last_listed = Instant::now();
                if Some(key) == list_key(&app) {
                    app.on_listing(rows);
                } else {
                    // Stale: the kind, scope or selector changed meanwhile.
                    app.request_rows();
                }
            }
            Some((context, version)) = ver_rx.recv() => {
                // A probe of the cluster just switched away from is stale.
                if context == app.context_name {
                    app.k8s_version = version;
                    app.dirty = true;
                }
            }
            Some(switched) = ctx_rx.recv() => match switched {
                Ok((new_client, default_ns, new_context, new_user)) => {
                    pollers.abort();
                    fleet_clients.insert(&new_context, new_client.clone()).await;
                    // Moving within a fleet keeps the fleet's scope; leaving
                    // for a single context lands on its own namespace.
                    let new_scope = if app.fleet.is_some() {
                        app.scope.clone()
                    } else {
                        Scope::Namespace(default_ns)
                    };
                    pollers = Pollers::spawn(
                        &new_client,
                        &new_scope,
                        &app.config,
                        inv_tx.clone(),
                        met_tx.clone(),
                        evt_tx.clone(),
                    );
                    spawn_discovery(&new_client, type_tx.clone());
                    spawn_version(&new_client, &new_context, ver_tx.clone());
                    app.adopt_context(new_client, new_scope, new_context, new_user, "…".into());
                    listing = None;
                }
                Err((context, err)) => app.set_status(
                    format!("could not switch to {context}: {err}"),
                    app::StatusKind::Error,
                ),
            },
            Some((node, service, result)) = node_rx.recv() => {
                app.on_node_logs(node, service, result);
            }
            Some(result) = desc_rx.recv() => app.on_describe(result),
            Some(result) = vol_rx.recv() => app.on_volume_usage(result),
            _ = tokio::time::sleep_until(next_frame), if app.dirty => {}
            _ = ticker.tick() => {}
            _ = &mut ctrl_c => app.should_quit = true,
        }

        if app.should_quit {
            break;
        }

        // A context switch rebuilds the client and everything hanging off it;
        // the result comes back on `ctx_rx`.
        if let Some(context) = app.pending_context.take() {
            let tx = ctx_tx.clone();
            tokio::spawn(async move {
                let result = k8s::connect(Some(&context))
                    .await
                    .map_err(|err| (context, format!("{err:#}")));
                let _ = tx.send(result).await;
            });
        }

        // A namespace change re-scopes the pollers without touching the client.
        if std::mem::take(&mut app.pollers_stale) {
            pollers.abort();
            pollers = Pollers::spawn(
                &app.client,
                &app.scope,
                &app.config,
                inv_tx.clone(),
                met_tx.clone(),
                evt_tx.clone(),
            );
        }

        // Service a pending describe request.
        if let Some((kind, namespace, name)) = app.take_describe_request() {
            let client = app.client.clone();
            let tx = desc_tx.clone();
            tokio::spawn(async move {
                let result = k8s::resources::get(&client, &kind, &namespace, &name)
                    .await
                    .map_err(|e| format!("describing {name}: {e}"));
                let _ = tx.send(result).await;
            });
        }

        // Refresh PVC usage when the volumes table asks for it.
        if app.take_volume_usage_request() {
            let client = app.client.clone();
            let nodes: Vec<String> = app.inventory.nodes.iter().map(|n| n.name.clone()).collect();
            let tx = vol_tx.clone();
            tokio::spawn(async move {
                let result = k8s::volume_stats::collect(&client, &nodes)
                    .await
                    .map_err(|e| e.to_string());
                let _ = tx.send(result).await;
            });
        }

        // Service a pending node journal request.
        if let Some((node, service)) = app.take_node_log_request() {
            let client = app.client.clone();
            let tail = app.config.logs.tail_lines;
            let tx = node_tx.clone();
            tokio::spawn(async move {
                let result =
                    k8s::node_logs::fetch(&client, &node, service, Some(tail).filter(|t| *t > 0))
                        .await
                        .map_err(|e| format!("{service} logs from {node}: {e}"));
                let _ = tx.send((node, service, result)).await;
            });
        }

        // Keep the browsed list live. Only while browsing: in the detail view
        // the selected row *is* the object on screen, and a refresh that
        // dropped it would silently switch what you are looking at.
        if app.right == app::RightMode::Browse
            && listing.is_none()
            && !app.loading
            && last_listed.elapsed() >= list_refresh
        {
            app.loading = true;
        }

        // Service a pending list request. Guarded so holding `Ctrl-r` cannot
        // pile up overlapping requests against the API server.
        if app.loading && listing.is_none() {
            if let (Some(kind), Some(key)) = (app.current_type().cloned(), list_key(&app)) {
                listing = Some(key.clone());
                let client = app.client.clone();
                let scope = app.scope.clone();
                let selector = app.selector_text.clone();
                let tx = row_tx.clone();
                let members = app.fleet.as_ref().map(|f| f.members.clone());
                let clients = fleet_clients.clone();
                tokio::spawn(async move {
                    let result = match members {
                        Some(members) => Ok(k8s::fleet::list(
                            &clients, &members, &kind, &scope, &selector,
                        )
                        .await),
                        None => k8s::resources::list(&client, &kind, &scope, &selector)
                            .await
                            .map(|rows| Listing {
                                rows,
                                failed: Vec::new(),
                            })
                            .map_err(|e| format!("listing {}: {e}", kind.name())),
                    };
                    let _ = tx.send((key, result)).await;
                });
            } else {
                app.loading = false;
            }
        }

        // Coalesce bursts: a pod emitting 50k lines/s still redraws at most
        // `max_fps` times per second.
        if app.dirty && last_draw.elapsed() >= frame_budget {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            app.dirty = false;
            last_draw = Instant::now();
        }
    }

    pollers.abort();
    app.detach_all();
    Ok(())
}

/// What a list request is for. Two requests with the same key would return
/// the same rows.
fn list_key(app: &App) -> Option<String> {
    let kind = app.current_type()?;
    // A fleet's listing does not depend on which member is connected, so
    // moving between members does not throw an in-flight answer away.
    let source = match &app.fleet {
        Some(fleet) => format!("fleet:{}", fleet.name),
        None => app.context_name.clone(),
    };
    Some(format!(
        "{}|{}|{}|{}",
        source,
        kind.qualified(),
        app.scope.label(),
        app.selector_text
    ))
}

/// The apiserver version is decoration for the header; never wait for it.
fn spawn_version(
    client: &kube::Client,
    context: &str,
    tx: tokio::sync::mpsc::Sender<(String, String)>,
) {
    let client = client.clone();
    let context = context.to_string();
    tokio::spawn(async move {
        let version = k8s::server_version(&client).await;
        let _ = tx.send((context, version)).await;
    });
}

/// Discovery is a burst of requests, so it runs off the UI thread and reports
/// back when it is done. A failure just leaves the palette empty.
fn spawn_discovery(client: &kube::Client, tx: tokio::sync::mpsc::Sender<Vec<ResourceType>>) {
    let client = client.clone();
    tokio::spawn(async move {
        if let Ok(types) = k8s::resources::discover(&client).await {
            let _ = tx.send(types).await;
        }
    });
}
