//! Command line interface.

use std::path::PathBuf;

use clap::Parser;

/// A fast, read-only Kubernetes TUI for logs and live metrics.
#[derive(Debug, Parser)]
#[command(name = "kscope", version, about, long_about = None)]
pub struct Cli {
    /// Namespace to watch. Defaults to the namespace of the current context.
    #[arg(short, long, env = "KSCOPE_NAMESPACE")]
    pub namespace: Option<String>,

    /// Watch every namespace the credentials allow.
    #[arg(short = 'A', long, conflicts_with = "namespace")]
    pub all_namespaces: bool,

    /// kubeconfig context to use.
    #[arg(long, env = "KSCOPE_CONTEXT")]
    pub context: Option<String>,

    /// Browse several contexts as one: a fleet name from the config file, or
    /// comma-separated context globs (`stg-*,qa-eu`). Rows from every member
    /// are listed together, tagged with their context.
    #[arg(long, value_name = "NAME|GLOBS", env = "KSCOPE_FLEET")]
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
    #[arg(short = 'l', long, value_name = "SELECTOR")]
    pub selector: Option<String>,

    /// Path to a kscope config file.
    #[arg(short, long, value_name = "FILE", env = "KSCOPE_CONFIG")]
    pub config: Option<PathBuf>,

    /// Historical lines to request when attaching. 0 (the default) means the
    /// container's entire retained history, from when it started.
    #[arg(long, value_name = "N")]
    pub tail: Option<i64>,

    /// Maximum lines held in memory. 0 (the default) means unlimited, so you
    /// can always scroll back to the start of the session.
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

impl Cli {
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
    fn parses_fleet_search() {
        let cli = Cli::parse_from([
            "kscope", "--fleet", "stg-*", "-k", "deploy", "--find", "api",
        ]);
        assert_eq!(cli.fleet.as_deref(), Some("stg-*"));
        assert_eq!(cli.kind.as_deref(), Some("deploy"));
        assert_eq!(cli.find.as_deref(), Some("api"));
    }
}
