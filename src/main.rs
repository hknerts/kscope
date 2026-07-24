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

    let (client, default_ns, context_name, user_name) = k8s::connect(cli.context.as_deref())
        .await
        .context("could not reach the cluster")?;

    let scope = if cli.all_namespaces {
        Scope::AllNamespaces
    } else {
        Scope::Namespace(
            cli.namespace
                .clone()
                .or_else(|| config.general.namespace.clone())
                .unwrap_or(default_ns),
        )
    };

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
    )
    .await;
    restore_terminal(&mut terminal)?;
    result
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

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
) -> Result<()> {
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(1024);
    let (inv_tx, mut inv_rx) = tokio::sync::mpsc::channel(16);
    let (met_tx, mut met_rx) = tokio::sync::mpsc::channel(16);
    let (evt_tx, mut evt_rx) = tokio::sync::mpsc::channel(16);
    // Resource listings are one-shot requests rather than a poller, so they
    // get their own channel and a task spawned per refresh.
    let (row_tx, mut row_rx) = tokio::sync::mpsc::channel::<Result<Vec<ResourceRow>, String>>(8);
    let (type_tx, mut type_rx) = tokio::sync::mpsc::channel::<Vec<ResourceType>>(4);

    let frame_budget = Duration::from_millis(1000 / config.general.max_fps.max(1) as u64);

    let mut pollers = Pollers::spawn(
        &client,
        &scope,
        &config,
        inv_tx.clone(),
        met_tx.clone(),
        evt_tx.clone(),
    );

    let k8s_version = k8s::server_version(&client).await;
    let contexts = k8s::resources::contexts()
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
        k8s_version,
        contexts,
    );

    spawn_discovery(&app.client, type_tx.clone());

    // Draw once immediately so the user sees the shell before the first poll.
    terminal.draw(|f| ui::draw(f, &mut app))?;
    let mut last_draw = Instant::now();
    let mut ticker = tokio::time::interval(frame_budget);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `loading` is edge-triggered: the app raises it, we service it once.
    let mut listing = false;

    loop {
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
            Some(rows) = row_rx.recv() => {
                listing = false;
                app.on_rows(rows);
            }
            _ = ticker.tick() => {}
            _ = tokio::signal::ctrl_c() => app.should_quit = true,
        }

        if app.should_quit {
            break;
        }

        // A context switch rebuilds the client and everything hanging off it.
        if let Some(context) = app.pending_context.take() {
            match k8s::connect(Some(&context)).await {
                Ok((new_client, default_ns, new_context, new_user)) => {
                    pollers.abort();
                    let new_scope = Scope::Namespace(default_ns);
                    let version = k8s::server_version(&new_client).await;
                    pollers = Pollers::spawn(
                        &new_client,
                        &new_scope,
                        &app.config,
                        inv_tx.clone(),
                        met_tx.clone(),
                        evt_tx.clone(),
                    );
                    spawn_discovery(&new_client, type_tx.clone());
                    app.adopt_context(new_client, new_scope, new_context, new_user, version);
                    listing = false;
                }
                Err(err) => app.set_status(
                    format!("could not switch to {context}: {err}"),
                    app::StatusKind::Error,
                ),
            }
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

        // Service a pending list request. Guarded so holding `Ctrl-r` cannot
        // pile up overlapping requests against the API server.
        if app.loading && !listing {
            if let Some(kind) = app.current_type().cloned() {
                listing = true;
                let client = app.client.clone();
                let scope = app.scope.clone();
                let tx = row_tx.clone();
                tokio::spawn(async move {
                    let result = k8s::resources::list(&client, &kind, &scope)
                        .await
                        .map_err(|e| format!("listing {}: {e}", kind.name()));
                    let _ = tx.send(result).await;
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
