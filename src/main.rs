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
use app::App;
use config::Config;
use k8s::discovery::Scope;

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

    let (client, default_ns) = k8s::connect(cli.context.as_deref())
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
    let result = run(&mut terminal, config, client, scope).await;
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

async fn run(terminal: &mut Tui, config: Config, client: kube::Client, scope: Scope) -> Result<()> {
    let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(1024);
    let (inv_tx, mut inv_rx) = tokio::sync::mpsc::channel(16);
    let (met_tx, mut met_rx) = tokio::sync::mpsc::channel(16);

    let inventory_interval = Duration::from_millis(config.general.inventory_refresh_ms);
    let metrics_interval = Duration::from_millis(config.metrics.refresh_ms);
    let frame_budget = Duration::from_millis(1000 / config.general.max_fps.max(1) as u64);

    let inventory_task = k8s::discovery::spawn(
        client.clone(),
        scope.clone(),
        inventory_interval,
        inv_tx,
    );
    let metrics_task = k8s::metrics::spawn(
        client.clone(),
        scope.clone(),
        metrics_interval,
        met_tx,
    );

    let mut input = event::spawn();
    let mut app = App::new(config, client, scope, log_tx);

    // Draw once immediately so the user sees the shell before the first poll.
    terminal.draw(|f| ui::draw(f, &mut app))?;
    let mut last_draw = Instant::now();
    let mut ticker = tokio::time::interval(frame_budget);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;

            Some(ev) = input.recv() => app.on_event(ev),
            Some(update) = inv_rx.recv() => app.on_update(update),
            Some(batch) = log_rx.recv() => app.on_log(batch),
            Some(sample) = met_rx.recv() => app.on_metrics(sample),
            _ = ticker.tick() => {}
            _ = tokio::signal::ctrl_c() => app.should_quit = true,
        }

        if app.should_quit {
            break;
        }

        // Coalesce bursts: a pod emitting 50k lines/s still redraws at most
        // `max_fps` times per second.
        if app.dirty && last_draw.elapsed() >= frame_budget {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            app.dirty = false;
            last_draw = Instant::now();
        }
    }

    inventory_task.abort();
    metrics_task.abort();
    app.detach_all();
    Ok(())
}
