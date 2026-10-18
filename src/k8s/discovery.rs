//! Live inventory: namespaces, pods (with their containers), nodes and PVCs.
//!
//! Everything here is **watched**, not polled. Each kind has a reflector — one
//! paginated LIST on start, then a WATCH that streams only what changed — and a
//! cheap local rebuild turns the caches into an [`Inventory`] snapshot for the
//! UI. Re-listing every pod in the cluster every few seconds was the single
//! largest cost kscope put on an API server; a watch costs next to nothing
//! while the cluster is quiet, and delivers changes within a fraction of a
//! second instead of a whole refresh interval later.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::core::v1::{Namespace, Node, PersistentVolumeClaim, Pod};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{Api, PartialObjectMeta};
use kube::runtime::reflector;
use kube::runtime::watcher::{self, watcher};
use kube::runtime::WatchStreamExt;
use kube::{Resource, ResourceExt};
use tokio::sync::mpsc::Sender;

use super::watch::{drive, Flags};
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

/// How long to coalesce a burst of watch events (a rollout touches every pod
/// several times) into one snapshot.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Spawn the inventory task. It keeps running until the channel is closed or
/// the handle is aborted, which also tears every watch down.
///
/// `heartbeat` is how often a snapshot is sent even when nothing changed, so
/// derived values such as ages keep moving.
pub fn spawn(
    client: kube::Client,
    scope: Scope,
    heartbeat: Duration,
    tx: Sender<Update>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let scoped = |client: &kube::Client| -> (
            Api<Pod>,
            Api<PartialObjectMeta<ReplicaSet>>,
            Api<PersistentVolumeClaim>,
        ) {
            match &scope {
                Scope::AllNamespaces => (
                    Api::all(client.clone()),
                    Api::all(client.clone()),
                    Api::all(client.clone()),
                ),
                Scope::Namespace(ns) => (
                    Api::namespaced(client.clone(), ns),
                    Api::namespaced(client.clone(), ns),
                    Api::namespaced(client.clone(), ns),
                ),
            }
        };
        let (pods_api, rs_api, pvc_api) = scoped(&client);
        let cfg = watcher::Config::default();

        let (pods, pods_w) = reflector::store::<Pod>();
        let (replicasets, rs_w) = reflector::store::<PartialObjectMeta<ReplicaSet>>();
        let (nodes, nodes_w) = reflector::store::<Node>();
        let (namespaces, ns_w) = reflector::store::<PartialObjectMeta<Namespace>>();
        let (claims, pvc_w) = reflector::store::<PersistentVolumeClaim>();

        let pods_flags = Flags::default();
        let rs_flags = Flags::default();
        let nodes_flags = Flags::default();
        let ns_flags = Flags::default();
        let pvc_flags = Flags::default();
        let changed = Arc::new(AtomicBool::new(true));

        // Only the pod watch is essential; its failures reach the status bar.
        // The others are optional by design — a namespace-scoped token cannot
        // list nodes, and that must not break the tool — so they just retry.
        let warn_tx = tx.clone();
        let mut last_warning = String::new();
        let pod_errors = move |err: String| {
            if err != last_warning {
                last_warning = err.clone();
                let _ = warn_tx.try_send(Update::Warning(format!("inventory: {err}")));
            }
        };

        let pods_watch = drive(
            pods_w,
            watcher(pods_api, cfg.clone()).modify(|p| {
                p.managed_fields_mut().clear();
            }),
            pods_flags.clone(),
            changed.clone(),
            pod_errors,
        );
        let rs_watch = drive(
            rs_w,
            watcher(rs_api, cfg.clone()),
            rs_flags.clone(),
            changed.clone(),
            |_| {},
        );
        let nodes_watch = drive(
            nodes_w,
            watcher(Api::<Node>::all(client.clone()), cfg.clone()).modify(|n| {
                n.managed_fields_mut().clear();
                // Image lists are the bulk of a Node object and unused here.
                if let Some(status) = n.status.as_mut() {
                    status.images = None;
                }
            }),
            nodes_flags.clone(),
            changed.clone(),
            |_| {},
        );
        let ns_watch = drive(
            ns_w,
            watcher(
                Api::<PartialObjectMeta<Namespace>>::all(client.clone()),
                cfg.clone(),
            ),
            ns_flags.clone(),
            changed.clone(),
            |_| {},
        );
        let pvc_watch = drive(
            pvc_w,
            watcher(pvc_api, cfg).modify(|p| {
                p.managed_fields_mut().clear();
            }),
            pvc_flags.clone(),
            changed.clone(),
            |_| {},
        );

        let publish = async {
            let mut last_sent: Option<Instant> = None;
            loop {
                tokio::time::sleep(DEBOUNCE).await;
                // Nothing to say until the first pod LIST has landed: an empty
                // snapshot would read as "no pods" rather than "loading".
                if !pods_flags.is_ready() {
                    continue;
                }
                let due = last_sent.is_none_or(|t| t.elapsed() >= heartbeat);
                if !changed.swap(false, Ordering::AcqRel) && !due {
                    continue;
                }
                let inventory = build(
                    &pods.state(),
                    rs_flags.is_ready().then(|| replicasets.state()).as_deref(),
                    &nodes.state(),
                    ns_flags.is_ready().then(|| namespaces.state()).as_deref(),
                    &claims.state(),
                );
                if tx
                    .send(Update::Inventory(Box::new(inventory)))
                    .await
                    .is_err()
                {
                    return;
                }
                last_sent = Some(Instant::now());
            }
        };

        tokio::select! {
            _ = publish => {}
            _ = pods_watch => {}
            _ = rs_watch => {}
            _ = nodes_watch => {}
            _ = ns_watch => {}
            _ = pvc_watch => {}
        }
    })
}

/// Assemble a snapshot from the caches. Pure and local — no API calls.
///
/// `replicasets` and `namespaces` are `None` while their watch is not (or
/// cannot be) ready, so the fallbacks below kick in.
pub fn build(
    pod_objects: &[Arc<Pod>],
    replicasets: Option<&[Arc<PartialObjectMeta<ReplicaSet>>]>,
    node_objects: &[Arc<Node>],
    namespaces: Option<&[Arc<PartialObjectMeta<Namespace>>]>,
    claim_objects: &[Arc<PersistentVolumeClaim>],
) -> Inventory {
    let now = chrono::Utc::now();

    let mut pods: Vec<PodInfo> = Vec::with_capacity(pod_objects.len());
    let mut claim_users: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for pod in pod_objects {
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
    if let Some(replicasets) = replicasets {
        let mut deployment_of: BTreeMap<(&str, &str), &str> = BTreeMap::new();
        for rs in replicasets {
            let meta = rs.meta();
            if let (Some(ns), Some(name), Some(owner)) = (
                meta.namespace.as_deref(),
                meta.name.as_deref(),
                meta.owner_references
                    .as_ref()
                    .and_then(|refs| refs.iter().find(|o| o.kind == "Deployment")),
            ) {
                deployment_of.insert((ns, name), owner.name.as_str());
            }
        }
        for pod in &mut pods {
            if pod.owner_kind == "ReplicaSet" {
                if let Some(dep) =
                    deployment_of.get(&(pod.namespace.as_str(), pod.owner_name.as_str()))
                {
                    pod.owner_kind = "Deployment".to_string();
                    pod.owner_name = dep.to_string();
                }
            }
        }
    }

    let mut nodes: Vec<NodeInfo> = node_objects.iter().map(|n| convert_node(n)).collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));

    let mut namespaces: Vec<String> = match namespaces {
        Some(list) => list
            .iter()
            .filter_map(|ns| ns.meta().name.clone())
            .collect(),
        None => pods.iter().map(|p| p.namespace.clone()).collect(),
    };
    namespaces.sort();
    namespaces.dedup();

    let mut volumes: Vec<VolumeInfo> = claim_objects
        .iter()
        .map(|pvc| convert_pvc(pvc, &claim_users))
        .collect();
    volumes.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));

    Inventory {
        namespaces,
        pods,
        nodes,
        volumes,
    }
}

fn convert_pod(pod: &Pod, now: chrono::DateTime<chrono::Utc>) -> (PodInfo, Vec<String>) {
    let meta = &pod.metadata;
    let spec = pod.spec.as_ref();
    let status = pod.status.as_ref();

    let claims: Vec<String> = spec
        .and_then(|s| s.volumes.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|v| {
            v.persistent_volume_claim
                .as_ref()
                .map(|p| p.claim_name.clone())
        })
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
        .map(|t| now.timestamp() - t.0.as_second())
        .unwrap_or(0);

    let mut statuses: BTreeMap<&str, (bool, i32, String)> = BTreeMap::new();
    for cs in status
        .and_then(|s| s.container_statuses.as_ref())
        .into_iter()
        .flatten()
        .chain(
            status
                .and_then(|s| s.init_container_statuses.as_ref())
                .into_iter()
                .flatten(),
        )
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
        statuses.insert(cs.name.as_str(), (cs.ready, cs.restart_count, state));
    }

    let mut containers = Vec::new();
    for (init, c) in spec
        .and_then(|s| s.init_containers.as_ref())
        .into_iter()
        .flatten()
        .map(|c| (true, c))
        .chain(
            spec.into_iter()
                .flat_map(|s| s.containers.iter())
                .map(|c| (false, c)),
        )
    {
        let (ready, restarts, state) =
            statuses
                .get(c.name.as_str())
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
            name: c.name.clone(),
            image: c.image.clone().unwrap_or_default(),
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
        labels: meta.labels.clone().unwrap_or_default(),
        namespace: meta.namespace.clone().unwrap_or_default(),
        name: meta.name.clone().unwrap_or_default(),
        node: spec.and_then(|s| s.node_name.clone()).unwrap_or_default(),
        phase: status
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Unknown".into()),
        age_seconds,
        containers,
        owner_kind,
        owner_name,
    };
    (info, claims)
}

fn convert_pvc(
    pvc: &PersistentVolumeClaim,
    claim_users: &BTreeMap<(String, String), Vec<String>>,
) -> VolumeInfo {
    let meta = &pvc.metadata;
    let spec = pvc.spec.as_ref();
    let status = pvc.status.as_ref();

    let namespace = meta.namespace.clone().unwrap_or_default();
    let name = meta.name.clone().unwrap_or_default();

    let requested_bytes = spec
        .and_then(|s| s.resources.as_ref())
        .and_then(|r| r.requests.as_ref())
        .and_then(|m| m.get("storage"))
        .map(|q| parse_memory(&q.0))
        .unwrap_or(0.0);
    let capacity_bytes = status
        .and_then(|s| s.capacity.as_ref())
        .and_then(|m| m.get("storage"))
        .map(|q| parse_memory(&q.0))
        .unwrap_or(0.0);

    let used_by = claim_users
        .get(&(namespace.clone(), name.clone()))
        .cloned()
        .unwrap_or_default();

    VolumeInfo {
        phase: status
            .and_then(|s| s.phase.clone())
            .unwrap_or_else(|| "Unknown".into()),
        capacity_bytes,
        requested_bytes,
        storage_class: spec
            .and_then(|s| s.storage_class_name.clone())
            .unwrap_or_default(),
        access_modes: spec
            .and_then(|s| s.access_modes.as_ref())
            .map(|m| m.join(","))
            .unwrap_or_default(),
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

fn convert_node(node: &Node) -> NodeInfo {
    let meta = &node.metadata;
    let status = node.status.as_ref();
    let (cpu_capacity, mem_capacity) = status
        .and_then(|s| s.capacity.as_ref())
        .map(quantities)
        .unwrap_or((0.0, 0.0));
    let (cpu_allocatable, mem_allocatable) = status
        .and_then(|s| s.allocatable.as_ref())
        .map(quantities)
        .unwrap_or((0.0, 0.0));
    let ready = status
        .and_then(|s| s.conditions.as_ref())
        .map(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
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
        name: meta.name.clone().unwrap_or_default(),
        ready,
        version: status
            .and_then(|s| s.node_info.as_ref())
            .map(|i| i.kubelet_version.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn arc<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Arc<T> {
        Arc::new(serde_json::from_value(v).unwrap())
    }

    fn pod(ns: &str, name: &str, owner: Option<(&str, &str)>) -> Arc<Pod> {
        let owners = owner
            .map(|(kind, n)| {
                json!([{"apiVersion": "apps/v1", "kind": kind, "name": n,
                                     "uid": "u", "controller": true}])
            })
            .unwrap_or(json!([]));
        arc(json!({
            "metadata": {"namespace": ns, "name": name, "ownerReferences": owners},
            "spec": {"nodeName": "n1", "containers": [{"name": "app"}],
                     "volumes": [{"name": "d", "persistentVolumeClaim": {"claimName": "data"}}]},
            "status": {"phase": "Running",
                       "containerStatuses": [{"name": "app", "ready": true, "restartCount": 2,
                                              "image": "i", "imageID": "", "state": {"running": {}}}]}
        }))
    }

    #[test]
    fn builds_a_snapshot_from_the_caches() {
        let pods = vec![
            pod("shop", "web-abc-1", Some(("ReplicaSet", "web-abc"))),
            pod("shop", "lonely", None),
            pod("other", "x", None),
        ];
        let rs: Vec<Arc<PartialObjectMeta<ReplicaSet>>> = vec![arc(json!({
            "apiVersion": "apps/v1", "kind": "ReplicaSet",
            "metadata": {"namespace": "shop", "name": "web-abc",
                         "ownerReferences": [{"apiVersion": "apps/v1", "kind": "Deployment",
                                              "name": "web", "uid": "u"}]}
        }))];
        let pvcs: Vec<Arc<PersistentVolumeClaim>> = vec![arc(json!({
            "metadata": {"namespace": "shop", "name": "data"},
            "status": {"phase": "Bound", "capacity": {"storage": "1Gi"}}
        }))];

        let inv = build(&pods, Some(&rs), &[], None, &pvcs);

        // Sorted, and resolved past the ReplicaSet to the Deployment.
        let names: Vec<_> = inv.pods.iter().map(|p| p.key()).collect();
        assert_eq!(names, ["other/x", "shop/lonely", "shop/web-abc-1"]);
        let web = &inv.pods[2];
        assert_eq!(
            (web.owner_kind.as_str(), web.owner_name.as_str()),
            ("Deployment", "web")
        );
        assert_eq!(web.restarts(), 2);
        // No namespace watch: fall back to the namespaces pods live in.
        assert_eq!(inv.namespaces, ["other", "shop"]);
        // Claim users come from the pods mounting them.
        assert_eq!(inv.volumes[0].used_by, ["web-abc-1", "lonely"]);
        assert_eq!(inv.volumes[0].capacity_bytes, 1024.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn without_replicasets_pods_stay_grouped_under_them() {
        let pods = vec![pod("shop", "web-abc-1", Some(("ReplicaSet", "web-abc")))];
        let inv = build(&pods, None, &[], None, &[]);
        assert_eq!(inv.pods[0].owner_kind, "ReplicaSet");
    }
}
