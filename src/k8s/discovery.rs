//! Periodic inventory refresh: namespaces, pods (with their containers) and
//! nodes. Runs in its own task and pushes snapshots to the UI over a channel.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::core::v1::{Namespace, Node, PersistentVolumeClaim, Pod};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{Api, ListParams};
use tokio::sync::mpsc::Sender;

use super::{ContainerInfo, Inventory, NodeInfo, PodInfo, VolumeInfo};
use crate::metrics::{parse_cpu, parse_memory};

/// Which pods to watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    AllNamespaces,
    Namespace(String),
}

impl Scope {
    pub fn label(&self) -> &str {
        match self {
            Scope::AllNamespaces => "all",
            Scope::Namespace(ns) => ns,
        }
    }
}

/// Message sent from the background tasks to the UI.
#[derive(Debug)]
pub enum Update {
    Inventory(Box<Inventory>),
    /// Non-fatal problem worth surfacing in the status bar.
    Warning(String),
}

/// Spawn the inventory loop. It keeps running until the channel is closed.
pub fn spawn(
    client: kube::Client,
    scope: Scope,
    interval: Duration,
    tx: Sender<Update>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match collect(&client, &scope).await {
                Ok(inv) => {
                    if tx.send(Update::Inventory(Box::new(inv))).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    if tx
                        .send(Update::Warning(format!("inventory: {err}")))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    })
}

/// One inventory pass.
pub async fn collect(client: &kube::Client, scope: &Scope) -> Result<Inventory> {
    let pods_api: Api<Pod> = match scope {
        Scope::AllNamespaces => Api::all(client.clone()),
        Scope::Namespace(ns) => Api::namespaced(client.clone(), ns),
    };

    let lp = ListParams::default().limit(2000);
    let pod_list = pods_api.list(&lp).await?;
    let now = chrono::Utc::now();

    let mut pods: Vec<PodInfo> = Vec::with_capacity(pod_list.items.len());
    let mut claim_users: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for pod in pod_list.items {
        let (info, claims) = convert_pod(pod, now);
        for claim in claims {
            claim_users
                .entry((info.namespace.clone(), claim))
                .or_default()
                .push(info.name.clone());
        }
        pods.push(info);
    }
    pods.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));

    // Pods only point at their immediate ReplicaSet; resolve that up to the
    // owning Deployment so the sidebar can group by the workload a user
    // actually thinks about. Best-effort: a namespace-scoped token that can't
    // list ReplicaSets just leaves those pods grouped under "ReplicaSet".
    let rs_api: Api<ReplicaSet> = match scope {
        Scope::AllNamespaces => Api::all(client.clone()),
        Scope::Namespace(ns) => Api::namespaced(client.clone(), ns),
    };
    if let Ok(rs_list) = rs_api.list(&lp).await {
        let mut deployment_of: BTreeMap<(String, String), String> = BTreeMap::new();
        for rs in rs_list.items {
            let ns = rs.metadata.namespace.clone().unwrap_or_default();
            let name = rs.metadata.name.clone().unwrap_or_default();
            if let Some(owner) = rs
                .metadata
                .owner_references
                .as_ref()
                .and_then(|refs| refs.iter().find(|o| o.kind == "Deployment"))
            {
                deployment_of.insert((ns, name), owner.name.clone());
            }
        }
        for pod in &mut pods {
            if pod.owner_kind == "ReplicaSet" {
                if let Some(dep) =
                    deployment_of.get(&(pod.namespace.clone(), pod.owner_name.clone()))
                {
                    pod.owner_kind = "Deployment".to_string();
                    pod.owner_name = dep.clone();
                }
            }
        }
    }

    // Nodes and namespaces are optional: a namespace-scoped token cannot list
    // them, and that must not break the whole tool.
    let nodes = match Api::<Node>::all(client.clone()).list(&lp).await {
        Ok(list) => list.items.into_iter().map(convert_node).collect(),
        Err(_) => Vec::new(),
    };

    let mut namespaces: Vec<String> =
        match Api::<Namespace>::all(client.clone()).list(&lp).await {
            Ok(list) => list
                .items
                .into_iter()
                .filter_map(|ns| ns.metadata.name)
                .collect(),
            Err(_) => {
                let mut seen: Vec<String> =
                    pods.iter().map(|p| p.namespace.clone()).collect();
                seen.sort();
                seen.dedup();
                seen
            }
        };
    namespaces.sort();

    // PVCs are optional for the same reason nodes are: a namespaced viewer
    // token may not have list rights on them cluster-wide.
    let pvcs_api: Api<PersistentVolumeClaim> = match scope {
        Scope::AllNamespaces => Api::all(client.clone()),
        Scope::Namespace(ns) => Api::namespaced(client.clone(), ns),
    };
    let mut volumes: Vec<VolumeInfo> = match pvcs_api.list(&lp).await {
        Ok(list) => list
            .items
            .into_iter()
            .map(|pvc| convert_pvc(pvc, &claim_users))
            .collect(),
        Err(_) => Vec::new(),
    };
    volumes.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));

    Ok(Inventory {
        namespaces,
        pods,
        nodes,
        volumes,
    })
}

fn convert_pod(pod: Pod, now: chrono::DateTime<chrono::Utc>) -> (PodInfo, Vec<String>) {
    let meta = pod.metadata;
    let spec = pod.spec.unwrap_or_default();
    let status = pod.status.unwrap_or_default();

    let claims: Vec<String> = spec
        .volumes
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.persistent_volume_claim.map(|p| p.claim_name))
        .collect();

    let (owner_kind, owner_name) = meta
        .owner_references
        .as_ref()
        .and_then(|refs| refs.iter().find(|o| o.controller.unwrap_or(false)))
        .map(|o| (o.kind.clone(), o.name.clone()))
        .unwrap_or_default();

    let age_seconds = meta
        .creation_timestamp
        .as_ref()
        .map(|t| (now - t.0).num_seconds())
        .unwrap_or(0);

    let mut statuses: BTreeMap<String, (bool, i32, String)> = BTreeMap::new();
    for cs in status
        .container_statuses
        .into_iter()
        .flatten()
        .chain(status.init_container_statuses.into_iter().flatten())
    {
        let state = cs
            .state
            .as_ref()
            .map(|s| {
                if s.running.is_some() {
                    "Running".to_string()
                } else if let Some(w) = &s.waiting {
                    w.reason.clone().unwrap_or_else(|| "Waiting".into())
                } else if let Some(t) = &s.terminated {
                    t.reason.clone().unwrap_or_else(|| "Terminated".into())
                } else {
                    "Unknown".to_string()
                }
            })
            .unwrap_or_else(|| "Unknown".to_string());
        statuses.insert(cs.name.clone(), (cs.ready, cs.restart_count, state));
    }

    let mut containers = Vec::new();
    for (init, c) in spec
        .init_containers
        .into_iter()
        .flatten()
        .map(|c| (true, c))
        .chain(spec.containers.into_iter().map(|c| (false, c)))
    {
        let (ready, restarts, state) = statuses
            .get(&c.name)
            .cloned()
            .unwrap_or((false, 0, "Pending".to_string()));
        let (cpu_request, mem_request) = c
            .resources
            .as_ref()
            .and_then(|r| r.requests.as_ref())
            .map(quantities)
            .unwrap_or((0.0, 0.0));
        let (cpu_limit, mem_limit) = c
            .resources
            .as_ref()
            .and_then(|r| r.limits.as_ref())
            .map(quantities)
            .unwrap_or((0.0, 0.0));
        containers.push(ContainerInfo {
            name: c.name,
            image: c.image.unwrap_or_default(),
            ready,
            restarts,
            state,
            init,
            cpu_request,
            cpu_limit,
            mem_request,
            mem_limit,
        });
    }

    let info = PodInfo {
        namespace: meta.namespace.unwrap_or_default(),
        name: meta.name.unwrap_or_default(),
        node: spec.node_name.unwrap_or_default(),
        phase: status.phase.unwrap_or_else(|| "Unknown".into()),
        age_seconds,
        containers,
        owner_kind,
        owner_name,
    };
    (info, claims)
}

fn convert_pvc(
    pvc: PersistentVolumeClaim,
    claim_users: &BTreeMap<(String, String), Vec<String>>,
) -> VolumeInfo {
    let meta = pvc.metadata;
    let spec = pvc.spec.unwrap_or_default();
    let status = pvc.status.unwrap_or_default();

    let namespace = meta.namespace.unwrap_or_default();
    let name = meta.name.unwrap_or_default();

    let requested_bytes = spec
        .resources
        .as_ref()
        .and_then(|r| r.requests.as_ref())
        .and_then(|m| m.get("storage"))
        .map(|q| parse_memory(&q.0))
        .unwrap_or(0.0);
    let capacity_bytes = status
        .capacity
        .as_ref()
        .and_then(|m| m.get("storage"))
        .map(|q| parse_memory(&q.0))
        .unwrap_or(0.0);

    let used_by = claim_users
        .get(&(namespace.clone(), name.clone()))
        .cloned()
        .unwrap_or_default();

    VolumeInfo {
        phase: status.phase.unwrap_or_else(|| "Unknown".into()),
        capacity_bytes,
        requested_bytes,
        storage_class: spec.storage_class_name.unwrap_or_default(),
        access_modes: spec.access_modes.unwrap_or_default().join(","),
        used_by,
        namespace,
        name,
    }
}

fn quantities(map: &BTreeMap<String, Quantity>) -> (f64, f64) {
    let cpu = map.get("cpu").map(|q| parse_cpu(&q.0)).unwrap_or(0.0);
    let mem = map.get("memory").map(|q| parse_memory(&q.0)).unwrap_or(0.0);
    (cpu, mem)
}

fn convert_node(node: Node) -> NodeInfo {
    let meta = node.metadata;
    let status = node.status.unwrap_or_default();
    let (cpu_capacity, mem_capacity) = status
        .capacity
        .as_ref()
        .map(quantities)
        .unwrap_or((0.0, 0.0));
    let (cpu_allocatable, mem_allocatable) = status
        .allocatable
        .as_ref()
        .map(quantities)
        .unwrap_or((0.0, 0.0));
    let ready = status
        .conditions
        .as_ref()
        .map(|cs| {
            cs.iter()
                .any(|c| c.type_ == "Ready" && c.status == "True")
        })
        .unwrap_or(false);
    let roles = meta
        .labels
        .as_ref()
        .map(|l| {
            let mut roles: Vec<&str> = l
                .keys()
                .filter_map(|k| k.strip_prefix("node-role.kubernetes.io/"))
                .filter(|r| !r.is_empty())
                .collect();
            roles.sort_unstable();
            roles.join(",")
        })
        .unwrap_or_default();

    NodeInfo {
        name: meta.name.unwrap_or_default(),
        ready,
        version: status
            .node_info
            .map(|i| i.kubelet_version)
            .unwrap_or_default(),
        cpu_capacity,
        mem_capacity,
        cpu_allocatable,
        mem_allocatable,
        roles: if roles.is_empty() {
            "worker".into()
        } else {
            roles
        },
    }
}
