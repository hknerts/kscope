//! Command line interface.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// A fast, read-only Kubernetes TUI for logs and live metrics.
#[derive(Debug, Parser)]
#[command(name = "kscope", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Namespace to watch. Defaults to the namespace of the current context
    /// (to every namespace for a fleet or `get`).
    #[arg(short, long, global = true, env = "KSCOPE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Watch every namespace the credentials allow.
    #[arg(short = 'A', long, global = true, conflicts_with = "namespace")]
    pub all_namespaces: bool,

    /// kubeconfig context to use.
    #[arg(long, global = true, env = "KSCOPE_CONTEXT")]
    pub context: Option<String>,

    /// Browse several contexts as one: a fleet name from the config file, or
    /// comma-separated context patterns. `stg` alone means every context
    /// whose name contains "stg"; `stg-*,qa-eu` are globs. Rows from every
    /// member are listed together, tagged with their context.
    #[arg(
        short = 'u',
        long,
        global = true,
        value_name = "FLEET",
        env = "KSCOPE_FLEET"
    )]
    pub fleet: Option<String>,

    /// Resource type to start on (`deploy`, `svc`, a CRD plural…). Also the
    /// kind `--find` searches. Defaults to pods.
    #[arg(short = 'k', long, value_name = "KIND")]
    pub kind: Option<String>,

    /// Print every object of `--kind` whose namespace or name contains TEXT,
    /// across the fleet (or the current context), and exit. Searches all
    /// namespaces unless `-n` is given. Exits 1 when nothing matches.
    #[arg(long, value_name = "TEXT", conflicts_with = "dump")]
    pub find: Option<String>,

    /// Label selector, e.g. `app=api,tier!=batch`. Narrows both the browsed
    /// list and the pods a workload's logs are collected from.
    #[arg(short = 'l', long, global = true, value_name = "SELECTOR")]
    pub selector: Option<String>,

    /// Path to a kscope config file.
    #[arg(short, long, global = true, value_name = "FILE", env = "KSCOPE_CONFIG")]
    pub config: Option<PathBuf>,

    /// Historical lines to request when attaching. 0 (the default) means the
    /// container's entire retained history, from when it started.
    #[arg(long, value_name = "N")]
    pub tail: Option<i64>,

    /// Maximum lines held in memory (50000 by default); the oldest are
    /// evicted. 0 means unlimited, so you can always scroll back to the start
    /// of the session.
    #[arg(long, value_name = "N")]
    pub buffer: Option<usize>,

    /// Only fetch lines from the last N seconds (e.g. --since 3600).
    #[arg(long, value_name = "SECONDS")]
    pub since: Option<i64>,

    /// Metrics poll interval in milliseconds.
    #[arg(long, value_name = "MS")]
    pub refresh: Option<u64>,

    /// Request RFC3339 timestamps from the API server.
    #[arg(long)]
    pub timestamps: bool,

    /// Print logs for `pod` or `pod:container` to stdout and exit.
    /// Useful in scripts and CI; no terminal UI is started.
    #[arg(long, value_name = "POD[:CONTAINER]")]
    pub dump: Option<String>,

    /// Write diagnostic logs to this file (the TUI owns stderr).
    #[arg(long, value_name = "FILE", env = "KSCOPE_LOG_FILE")]
    pub log_file: Option<PathBuf>,
}

/// Non-interactive commands. Without one, kscope starts the TUI.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List objects of a kind, kubectl-style, across a fleet with `-u`:
    /// `kscope get pods -u stg`, `kscope get deploy checkout -u prod`.
    /// Spans every namespace unless `-n` is given.
    Get {
        /// Resource type: `pods`, `deploy`, `sts`, `svc`, a CRD plural…
        kind: String,
        /// Only objects whose name or namespace contains this.
        name: Option<String>,
    },
}

impl Cli {
    /// What to search for without starting the TUI: `(kind, text, exit 1
    /// when nothing matches)`. `get` behaves like kubectl and exits 0 on an
    /// empty result; `--find` behaves like grep.
    pub fn search(&self) -> Option<(String, String, bool)> {
        if let Some(Command::Get { kind, name }) = &self.command {
            return Some((kind.clone(), name.clone().unwrap_or_default(), false));
        }
        let text = self.find.clone()?;
        let kind = self.kind.clone().unwrap_or_else(|| "pods".to_string());
        Some((kind, text, true))
    }

    /// Split a `--dump pod:container` argument.
    pub fn dump_target(&self) -> Option<(String, Option<String>)> {
        self.dump.as_ref().map(|raw| match raw.split_once(':') {
            Some((pod, container)) => (pod.to_string(), Some(container.to_string())),
            None => (raw.clone(), None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn splits_dump_target() {
        let cli = Cli::parse_from(["kscope", "--dump", "api-0:app"]);
        assert_eq!(
            cli.dump_target(),
            Some(("api-0".into(), Some("app".into())))
        );
        let cli = Cli::parse_from(["kscope", "--dump", "api-0"]);
        assert_eq!(cli.dump_target(), Some(("api-0".into(), None)));
    }

    #[test]
    fn parses_get_with_a_fleet_either_side() {
        for argv in [
            &["kscope", "get", "pods", "-u", "stg"][..],
            &["kscope", "-u", "stg", "get", "pods"][..],
        ] {
            let cli = Cli::parse_from(argv);
            assert_eq!(cli.fleet.as_deref(), Some("stg"));
            assert_eq!(cli.search(), Some(("pods".into(), String::new(), false)));
        }
        let cli = Cli::parse_from([
            "kscope", "get", "deploy", "checkout", "-u", "prod", "-n", "shop",
        ]);
        assert_eq!(
            cli.search(),
            Some(("deploy".into(), "checkout".into(), false))
        );
        assert_eq!(cli.namespace.as_deref(), Some("shop"));
        assert_eq!(Cli::parse_from(["kscope"]).search(), None);
    }

    #[test]
    fn parses_fleet_search() {
        let cli = Cli::parse_from([
            "kscope", "--fleet", "stg-*", "-k", "deploy", "--find", "api",
        ]);
        assert_eq!(cli.fleet.as_deref(), Some("stg-*"));
        assert_eq!(cli.kind.as_deref(), Some("deploy"));
        assert_eq!(cli.find.as_deref(), Some("api"));
    }
}
