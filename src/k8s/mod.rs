//! Kubernetes access layer.
//!
//! kscope is deliberately **read-only**: it lists pods and nodes, streams logs
//! and reads `metrics.k8s.io`. It never creates, patches or deletes anything,
//! so it is safe to run against production with a view-only ServiceAccount.

pub mod discovery;
pub mod events;
pub mod fleet;
pub mod logs;
pub mod metrics;
pub mod node_logs;
pub mod resources;
pub mod selection;
pub mod volume_stats;
pub mod watch;

use std::future::Future;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use kube::config::{KubeConfigOptions, Kubeconfig};

/// Connect to the cluster, honouring `--context` and the usual kubeconfig or
/// in-cluster environment discovery.
///
/// Returns the client, the namespace configured for the context, and — best
/// effort, purely for display in the header — the context and kubeconfig
/// user names. Those are empty when running in-cluster, where there is no
/// kubeconfig to read them from.
pub async fn connect(context: Option<&str>) -> Result<(kube::Client, String, String, String)> {
    let config = match context {
        Some(ctx) => {
            let kubeconfig = Kubeconfig::read().context("reading kubeconfig")?;
            let options = KubeConfigOptions {
                context: Some(ctx.to_string()),
                ..Default::default()
            };
            kube::Config::from_custom_kubeconfig(kubeconfig, &options)
                .await
                .with_context(|| format!("loading kube context {ctx}"))?
        }
        None => kube::Config::infer()
            .await
            .context("inferring cluster configuration (kubeconfig or in-cluster)")?,
    };
    let namespace = config.default_namespace.clone();
    let client = kube::Client::try_from(config).context("building kubernetes client")?;

    let kubeconfig = Kubeconfig::read().ok();
    let context_name = context
        .map(str::to_string)
        .or_else(|| kubeconfig.as_ref().and_then(|k| k.current_context.clone()))
        .unwrap_or_else(|| "in-cluster".to_string());
    let user_name = kubeconfig
        .as_ref()
        .and_then(|k| k.contexts.iter().find(|c| c.name == context_name))
        .and_then(|c| c.context.as_ref())
        .and_then(|ctx| ctx.user.clone())
        .unwrap_or_default();

    Ok((client, namespace, context_name, user_name))
}

/// Ceiling on any one-shot request (a listing, a describe, discovery…).
///
/// kube's client has a connect timeout but no read timeout — it cannot, as
/// watches and log streams legitimately stay open for hours. So an API server
/// that accepts the connection and then never answers would hang a request
/// forever, and because kscope never overlaps two listings, the list would
/// silently stop refreshing. Every one-shot call goes through [`bounded`].
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Connecting can run an exec credential plugin, which may open a browser
/// for an SSO login; give a human time to finish it.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

/// Run `request`, failing it after `limit` instead of waiting indefinitely.
pub async fn bounded_by<T>(limit: Duration, request: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(limit, request)
        .await
        .unwrap_or_else(|_| {
            Err(anyhow!(
                "no answer from the API server in {}s",
                limit.as_secs()
            ))
        })
}

/// [`bounded_by`] with [`REQUEST_TIMEOUT`].
pub async fn bounded<T>(request: impl Future<Output = Result<T>>) -> Result<T> {
    bounded_by(REQUEST_TIMEOUT, request).await
}

/// Best-effort apiserver version string (e.g. "v1.30.2"), for the header.
/// Never fails the whole app — an unreachable `/version` endpoint just shows
/// as "unknown".
pub async fn server_version(client: &kube::Client) -> String {
    bounded(async { Ok(client.apiserver_version().await?) })
        .await
        .map(|info| info.git_version)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_request_that_never_answers_fails_instead_of_hanging() {
        let never = std::future::pending::<Result<()>>();
        let err = bounded_by(Duration::from_secs(5), never).await.unwrap_err();
        assert!(err.to_string().contains("no answer"), "{err}");
    }

    #[tokio::test]
    async fn an_answer_passes_through() {
        assert_eq!(bounded(async { Ok(7) }).await.unwrap(), 7);
        let err = bounded(async { Err::<(), _>(anyhow!("forbidden")) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "forbidden");
    }
}

/// A container inside a pod, as seen by the inventory poller.
#[derive(Debug, Clone, Default)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub ready: bool,
    pub restarts: i32,
    pub state: String,
    pub init: bool,
    pub cpu_request: f64,
    pub cpu_limit: f64,
    pub mem_request: f64,
    pub mem_limit: f64,
}

/// A pod, as seen by the inventory poller.
#[derive(Debug, Clone, Default)]
pub struct PodInfo {
    pub namespace: String,
    pub name: String,
    pub node: String,
    pub phase: String,
    pub age_seconds: i64,
    pub containers: Vec<ContainerInfo>,
    /// Owning workload kind (e.g. "Deployment", "StatefulSet", "DaemonSet",
    /// "Job"), resolved past the intermediate ReplicaSet where relevant.
    /// Empty for standalone pods.
    pub owner_kind: String,
    /// Owning workload name. Empty for standalone pods.
    pub owner_name: String,
    /// Pod labels, so a Service's selector (and `--selector`) can be matched
    /// against them without a second API call.
    pub labels: std::collections::BTreeMap<String, String>,
}

impl PodInfo {
    pub fn key(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    pub fn ready(&self) -> (usize, usize) {
        let total = self.containers.iter().filter(|c| !c.init).count();
        let ready = self
            .containers
            .iter()
            .filter(|c| !c.init && c.ready)
            .count();
        (ready, total)
    }

    pub fn restarts(&self) -> i32 {
        self.containers.iter().map(|c| c.restarts).sum()
    }

    pub fn healthy(&self) -> bool {
        let (ready, total) = self.ready();
        matches!(self.phase.as_str(), "Running" | "Succeeded") && (total == 0 || ready == total)
    }
}

/// A PersistentVolumeClaim, as seen by the inventory poller. Capacity only —
/// kscope reads the Kubernetes API objects, not kubelet's stats/summary, so
/// there is no live "bytes actually used" figure here.
#[derive(Debug, Clone, Default)]
pub struct VolumeInfo {
    pub namespace: String,
    pub name: String,
    pub phase: String,
    pub capacity_bytes: f64,
    pub requested_bytes: f64,
    pub storage_class: String,
    pub access_modes: String,
    pub used_by: Vec<String>,
}

impl VolumeInfo {
    pub fn key(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

/// A node, as seen by the inventory poller.
#[derive(Debug, Clone, Default)]
pub struct NodeInfo {
    pub name: String,
    pub ready: bool,
    pub version: String,
    pub cpu_capacity: f64,
    pub mem_capacity: f64,
    pub cpu_allocatable: f64,
    pub mem_allocatable: f64,
    pub roles: String,
}

/// One inventory refresh: everything the sidebar and metric tables need.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub namespaces: Vec<String>,
    pub pods: Vec<PodInfo>,
    pub nodes: Vec<NodeInfo>,
    pub volumes: Vec<VolumeInfo>,
}

/// Format an age in seconds the way kubectl does.
pub fn fmt_age(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{}h", s / 86_400, (s % 86_400) / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_age_like_kubectl() {
        assert_eq!(fmt_age(30), "30s");
        assert_eq!(fmt_age(90), "1m");
        assert_eq!(fmt_age(3_700), "1h1m");
        assert_eq!(fmt_age(90_000), "1d1h");
    }

    #[test]
    fn pod_readiness() {
        let pod = PodInfo {
            phase: "Running".into(),
            containers: vec![
                ContainerInfo {
                    ready: true,
                    ..Default::default()
                },
                ContainerInfo {
                    ready: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(pod.ready(), (1, 2));
        assert!(!pod.healthy());
    }
}
