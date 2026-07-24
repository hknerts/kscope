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
}
