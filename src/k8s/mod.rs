//! Kubernetes access layer.
//!
//! kscope is deliberately **read-only**: it lists pods and nodes, streams logs
//! and reads `metrics.k8s.io`. It never creates, patches or deletes anything,
//! so it is safe to run against production with a view-only ServiceAccount.

pub mod discovery;
pub mod logs;
pub mod metrics;

use anyhow::{Context, Result};
use kube::config::{KubeConfigOptions, Kubeconfig};

/// Connect to the cluster, honouring `--context` and the usual kubeconfig or
/// in-cluster environment discovery.
///
/// Returns the client alongside the namespace configured for the context.
pub async fn connect(context: Option<&str>) -> Result<(kube::Client, String)> {
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
    Ok((client, namespace))
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
