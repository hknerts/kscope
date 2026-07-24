//! Live metrics collection from the `metrics.k8s.io` aggregated API.
//!
//! The metrics types are not part of `k8s-openapi`, so they are read as
//! [`DynamicObject`]s and decoded by hand. That also means kscope keeps working
//! when the cluster serves a different `v1betaX` revision.

use std::time::Duration;

use anyhow::{anyhow, Result};
use kube::api::{Api, ListParams};
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use tokio::sync::mpsc::Sender;

use super::discovery::Scope;
use crate::metrics::{parse_cpu, parse_memory};

/// Usage of a single container at one point in time.
#[derive(Debug, Clone)]
pub struct ContainerSample {
    pub name: String,
    pub cpu_milli: f64,
    pub mem_bytes: f64,
}

#[derive(Debug, Clone)]
pub struct PodSample {
    pub namespace: String,
    pub name: String,
    pub containers: Vec<ContainerSample>,
}

#[derive(Debug, Clone)]
pub struct NodeSample {
    pub name: String,
    pub cpu_milli: f64,
    pub mem_bytes: f64,
}

/// One complete poll.
#[derive(Debug, Default)]
pub struct MetricsSnapshot {
    pub pods: Vec<PodSample>,
    pub nodes: Vec<NodeSample>,
}

#[derive(Debug)]
pub enum MetricsEvent {
    Snapshot(Box<MetricsSnapshot>),
    /// metrics-server missing or forbidden — shown once in the metrics pane.
    Unavailable(String),
}

fn pod_metrics_resource() -> ApiResource {
    let gvk = GroupVersionKind::gvk("metrics.k8s.io", "v1beta1", "PodMetrics");
    ApiResource::from_gvk_with_plural(&gvk, "pods")
}

fn node_metrics_resource() -> ApiResource {
    let gvk = GroupVersionKind::gvk("metrics.k8s.io", "v1beta1", "NodeMetrics");
    ApiResource::from_gvk_with_plural(&gvk, "nodes")
}

/// Spawn the polling loop.
pub fn spawn(
    client: kube::Client,
    scope: Scope,
    interval: Duration,
    tx: Sender<MetricsEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reported_failure = false;
        loop {
            ticker.tick().await;
            match poll(&client, &scope).await {
                Ok(snapshot) => {
                    reported_failure = false;
                    if tx.send(MetricsEvent::Snapshot(Box::new(snapshot))).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    if !reported_failure {
                        reported_failure = true;
                        if tx
                            .send(MetricsEvent::Unavailable(err.to_string()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        }
    })
}

/// Perform a single poll of both pod and node metrics.
pub async fn poll(client: &kube::Client, scope: &Scope) -> Result<MetricsSnapshot> {
    let pod_ar = pod_metrics_resource();
    let node_ar = node_metrics_resource();
    let lp = ListParams::default();

    let pods_api: Api<DynamicObject> = match scope {
        Scope::AllNamespaces => Api::all_with(client.clone(), &pod_ar),
        Scope::Namespace(ns) => Api::namespaced_with(client.clone(), ns, &pod_ar),
    };
    let nodes_api: Api<DynamicObject> = Api::all_with(client.clone(), &node_ar);

    let (pods_res, nodes_res) = tokio::join!(pods_api.list(&lp), nodes_api.list(&lp));

    let pod_list = pods_res.map_err(|e| {
        anyhow!("metrics.k8s.io unavailable ({e}); is metrics-server installed?")
    })?;

    let mut pods = Vec::with_capacity(pod_list.items.len());
    for item in pod_list.items {
        if let Some(sample) = decode_pod(&item) {
            pods.push(sample);
        }
    }

    // Node metrics require cluster scope; tolerate a namespaced token.
    let nodes = match nodes_res {
        Ok(list) => list.items.iter().filter_map(decode_node).collect(),
        Err(_) => Vec::new(),
    };

    Ok(MetricsSnapshot { pods, nodes })
}

fn decode_pod(obj: &DynamicObject) -> Option<PodSample> {
    let name = obj.metadata.name.clone()?;
    let namespace = obj.metadata.namespace.clone().unwrap_or_default();
    let containers = obj
        .data
        .get("containers")?
        .as_array()?
        .iter()
        .filter_map(|c| {
            let usage = c.get("usage")?;
            Some(ContainerSample {
                name: c.get("name")?.as_str()?.to_string(),
                cpu_milli: parse_cpu(usage.get("cpu")?.as_str().unwrap_or("0")),
                mem_bytes: parse_memory(usage.get("memory")?.as_str().unwrap_or("0")),
            })
        })
        .collect();
    Some(PodSample {
        namespace,
        name,
        containers,
    })
}

fn decode_node(obj: &DynamicObject) -> Option<NodeSample> {
    let usage = obj.data.get("usage")?;
    Some(NodeSample {
        name: obj.metadata.name.clone()?,
        cpu_milli: parse_cpu(usage.get("cpu")?.as_str().unwrap_or("0")),
        mem_bytes: parse_memory(usage.get("memory")?.as_str().unwrap_or("0")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::DynamicObject;

    #[test]
    fn decodes_pod_metrics_payload() {
        let json = serde_json::json!({
            "apiVersion": "metrics.k8s.io/v1beta1",
            "kind": "PodMetrics",
            "metadata": { "name": "api-0", "namespace": "prod" },
            "containers": [
                { "name": "app", "usage": { "cpu": "142331825n", "memory": "331612Ki" } },
                { "name": "envoy", "usage": { "cpu": "12m", "memory": "64Mi" } }
            ]
        });
        let obj: DynamicObject = serde_json::from_value(json).unwrap();
        let sample = decode_pod(&obj).unwrap();
        assert_eq!(sample.namespace, "prod");
        assert_eq!(sample.containers.len(), 2);
        assert!((sample.containers[0].cpu_milli - 142.331825).abs() < 1e-4);
        assert_eq!(sample.containers[1].mem_bytes, 64.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn decodes_node_metrics_payload() {
        let json = serde_json::json!({
            "apiVersion": "metrics.k8s.io/v1beta1",
            "kind": "NodeMetrics",
            "metadata": { "name": "node-1" },
            "usage": { "cpu": "1500m", "memory": "4Gi" }
        });
        let obj: DynamicObject = serde_json::from_value(json).unwrap();
        let sample = decode_node(&obj).unwrap();
        assert_eq!(sample.cpu_milli, 1500.0);
        assert_eq!(sample.mem_bytes, 4.0 * 1024f64.powi(3));
    }
}
