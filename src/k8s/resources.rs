//! Generic resource browsing.
//!
//! Rather than hard-coding a list of kinds, kscope asks the cluster what it
//! serves and lists whatever the user picks as [`DynamicObject`]s. That way
//! `:` completes CRDs the same way it completes pods, and a cluster without
//! some API group simply does not offer it.

use std::collections::BTreeMap;

use anyhow::Result;
use kube::api::{Api, ListParams};
use kube::core::{ApiResource, DynamicObject, GroupVersionKind};
use kube::discovery::{Discovery, Scope as ApiScope};

use super::discovery::Scope;

/// One kind the cluster serves and that kscope can list.
#[derive(Debug, Clone)]
pub struct ResourceType {
    pub api: ApiResource,
    pub namespaced: bool,
}

impl ResourceType {
    /// Lower-case plural — the canonical name, and what the palette shows.
    pub fn name(&self) -> String {
        self.api.plural.to_ascii_lowercase()
    }

    /// The group-qualified name, used to disambiguate two kinds that share a
    /// plural (e.g. `events` in both the core and `events.k8s.io` groups).
    pub fn qualified(&self) -> String {
        if self.api.group.is_empty() {
            self.name()
        } else {
            format!("{}.{}", self.name(), self.api.group)
        }
    }

    /// Everything `:` should accept for this kind: the plural, the singular
    /// kind, the group-qualified plural, and the well-known kubectl short
    /// names. The discovery API does expose `shortNames`, but kube's
    /// [`ApiResource`] drops them, so the common ones are listed by hand.
    pub fn aliases(&self) -> Vec<String> {
        let plural = self.name();
        let kind = self.api.kind.to_ascii_lowercase();
        let mut out = vec![plural.clone()];
        if kind != plural {
            out.push(kind.clone());
        }
        let qualified = self.qualified();
        if qualified != plural {
            out.push(qualified);
        }
        if let Some(short) = short_name(&self.api.group, &plural) {
            out.push(short.to_string());
        }
        out
    }
}

/// kubectl's short names for the kinds people actually type.
fn short_name(group: &str, plural: &str) -> Option<&'static str> {
    let core = group.is_empty();
    Some(match (core, plural) {
        (true, "pods") => "po",
        (true, "services") => "svc",
        (true, "namespaces") => "ns",
        (true, "nodes") => "no",
        (true, "persistentvolumeclaims") => "pvc",
        (true, "persistentvolumes") => "pv",
        (true, "configmaps") => "cm",
        (true, "serviceaccounts") => "sa",
        (true, "endpoints") => "ep",
        (true, "events") => "ev",
        (true, "replicationcontrollers") => "rc",
        (true, "resourcequotas") => "quota",
        (true, "limitranges") => "limits",
        (_, "deployments") => "deploy",
        (_, "statefulsets") => "sts",
        (_, "daemonsets") => "ds",
        (_, "replicasets") => "rs",
        (_, "ingresses") => "ing",
        (_, "networkpolicies") => "netpol",
        (_, "cronjobs") => "cj",
        (_, "horizontalpodautoscalers") => "hpa",
        (_, "poddisruptionbudgets") => "pdb",
        (_, "storageclasses") => "sc",
        (_, "customresourcedefinitions") => "crd",
        (_, "priorityclasses") => "pc",
        _ => return None,
    })
}

/// Ask the cluster what it serves. Only kinds that can actually be listed are
/// returned, so the palette never offers something that will error on Enter.
pub async fn discover(client: &kube::Client) -> Result<Vec<ResourceType>> {
    let discovery = Discovery::new(client.clone()).run().await?;
    let mut out = Vec::new();
    for group in discovery.groups() {
        for (api, caps) in group.recommended_resources() {
            if !caps.operations.iter().any(|op| op == "list") {
                continue;
            }
            out.push(ResourceType {
                namespaced: caps.scope == ApiScope::Namespaced,
                api,
            });
        }
    }
    // Stable, predictable ordering: core group first, then alphabetical.
    let rank = |t: &ResourceType| (if t.api.group.is_empty() { 0 } else { 1 }, t.name());
    out.sort_by_key(rank);
    out.dedup_by(|a, b| a.qualified() == b.qualified());
    Ok(out)
}

/// Find the type a `:` selector refers to. Matches any of its aliases.
pub fn resolve<'a>(types: &'a [ResourceType], selector: &str) -> Option<&'a ResourceType> {
    let needle = selector.trim().to_ascii_lowercase();
    types.iter().find(|t| t.aliases().contains(&needle))
}

/// One listed object, flattened to what a table needs.
#[derive(Debug, Clone, Default)]
pub struct ResourceRow {
    pub namespace: String,
    pub name: String,
    pub age_seconds: i64,
    /// Best-effort status, e.g. a pod's phase or a deployment's ready count.
    pub status: String,
    /// True when `status` reads as a problem, so the table can colour it and
    /// the "problems only" filter can pick it out.
    pub unhealthy: bool,
    /// The object's own pod selector, when it has one. Lets a Service (or a
    /// CRD shaped like one) resolve to the pods whose logs it fronts.
    pub selector: std::collections::BTreeMap<String, String>,
}

impl ResourceRow {
    pub fn key(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.namespace, self.name)
        }
    }
}

/// List every object of `kind` in `scope`.
pub async fn list(
    client: &kube::Client,
    kind: &ResourceType,
    scope: &Scope,
    selector: &str,
) -> Result<Vec<ResourceRow>> {
    let api: Api<DynamicObject> = match (kind.namespaced, scope) {
        (false, _) | (true, Scope::AllNamespaces) => Api::all_with(client.clone(), &kind.api),
        (true, Scope::Namespace(ns)) => Api::namespaced_with(client.clone(), ns, &kind.api),
    };
    let mut lp = ListParams::default().limit(1000);
    // Let the API server do the filtering when a selector is set — it is both
    // cheaper and the only way to narrow a list capped at 1000 items.
    if !selector.trim().is_empty() {
        lp = lp.labels(selector.trim());
    }
    let list = api.list(&lp).await?;
    let now = chrono::Utc::now().timestamp();

    let mut rows: Vec<ResourceRow> = list.items.iter().map(|o| convert(o, now)).collect();
    rows.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));
    Ok(rows)
}

/// Fetch one object in full, for the describe tab.
///
/// Listings drop everything but the few columns a table needs, so describing
/// re-fetches — one request when the tab is opened, rather than carrying every
/// object's whole spec in memory.
pub async fn get(
    client: &kube::Client,
    kind: &ResourceType,
    namespace: &str,
    name: &str,
) -> Result<serde_json::Value> {
    let api: Api<DynamicObject> = if kind.namespaced && !namespace.is_empty() {
        Api::namespaced_with(client.clone(), namespace, &kind.api)
    } else {
        Api::all_with(client.clone(), &kind.api)
    };
    let object = api.get(name).await?;
    Ok(serde_json::to_value(&object)?)
}

fn convert(obj: &DynamicObject, now: i64) -> ResourceRow {
    let age_seconds = obj
        .metadata
        .creation_timestamp
        .as_ref()
        .map(|t| (now - t.0.as_second()).max(0))
        .unwrap_or(0);
    let (status, unhealthy) = summarise(&obj.data);
    ResourceRow {
        namespace: obj.metadata.namespace.clone().unwrap_or_default(),
        name: obj.metadata.name.clone().unwrap_or_default(),
        age_seconds,
        status,
        unhealthy,
        selector: pod_selector(&obj.data),
    }
}

/// A Service's `spec.selector`, or a workload's `spec.selector.matchLabels`.
/// Anything else yields an empty map, which selects nothing.
fn pod_selector(data: &serde_json::Value) -> BTreeMap<String, String> {
    let node = &data["spec"]["selector"];
    let map = if node["matchLabels"].is_object() {
        &node["matchLabels"]
    } else {
        node
    };
    map.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// A one-word health summary from whatever shape the object happens to have.
/// Deliberately generic: it works on CRDs that follow the usual conventions
/// and falls back to an empty string rather than guessing wrong.
fn summarise(data: &serde_json::Value) -> (String, bool) {
    let status = &data["status"];

    // Pods first, and not by phase: a pod whose container is stuck in
    // CrashLoopBackOff still reports phase "Running", which is exactly the
    // case triage needs to catch. The container states are the real story.
    if let Some(containers) = status["containerStatuses"].as_array() {
        if let Some(reason) = containers
            .iter()
            .filter_map(|c| {
                c["state"]["waiting"]["reason"]
                    .as_str()
                    .or_else(|| c["state"]["terminated"]["reason"].as_str())
            })
            // "ContainerCreating" and "Completed" are not problems.
            .find(|r| !matches!(*r, "ContainerCreating" | "PodInitializing" | "Completed"))
        {
            return (reason.to_string(), true);
        }
        let ready = containers.iter().filter(|c| c["ready"] == true).count();
        let total = containers.len();
        let restarts: i64 = containers
            .iter()
            .filter_map(|c| c["restartCount"].as_i64())
            .sum();
        let phase = status["phase"].as_str().unwrap_or("Unknown");
        if ready < total {
            return (format!("{ready}/{total} {phase}"), true);
        }
        // Healthy, but a restart count worth noticing is still surfaced.
        if restarts > 0 {
            return (format!("{phase} ↺{restarts}"), false);
        }
        return (
            phase.to_string(),
            phase != "Running" && phase != "Succeeded",
        );
    }

    // Everything else with a phase: PVCs, namespaces, and pods that have no
    // container statuses yet (Pending, unschedulable).
    if let Some(phase) = status["phase"].as_str() {
        let bad = !matches!(phase, "Running" | "Succeeded" | "Bound" | "Active");
        return (phase.to_string(), bad);
    }

    // Workloads: ready vs desired. `replicas` is the desired count on
    // Deployments/StatefulSets, so a missing `readyReplicas` means zero.
    let desired = data["spec"]["replicas"]
        .as_i64()
        .or_else(|| status["replicas"].as_i64());
    if let Some(desired) = desired {
        let ready = status["readyReplicas"].as_i64().unwrap_or(0);
        return (format!("{ready}/{desired}"), ready < desired);
    }
    // DaemonSets count nodes rather than replicas.
    if let Some(desired) = status["desiredNumberScheduled"].as_i64() {
        let ready = status["numberReady"].as_i64().unwrap_or(0);
        return (format!("{ready}/{desired}"), ready < desired);
    }

    // Anything with conditions: Ready, or the first condition that is not.
    if let Some(conditions) = status["conditions"].as_array() {
        let ready = conditions
            .iter()
            .find(|c| c["type"] == "Ready" || c["type"] == "Available");
        if let Some(c) = ready {
            let ok = c["status"] == "True";
            let type_ = c["type"].as_str().unwrap_or("Ready");
            return (
                if ok {
                    type_.to_string()
                } else {
                    format!("Not{type_}")
                },
                !ok,
            );
        }
    }

    (String::new(), false)
}

/// The `PodMetrics`/`NodeMetrics` GVKs are not part of discovery's recommended
/// set on every cluster, so the metrics view keeps building them by hand.
pub fn gvk(group: &str, version: &str, kind: &str) -> GroupVersionKind {
    GroupVersionKind::gvk(group, version, kind)
}

/// kubeconfig context names, for the left-hand pane. Best effort: running
/// in-cluster there is no kubeconfig, and that is not an error.
pub fn contexts() -> BTreeMap<String, String> {
    let Ok(config) = kube::config::Kubeconfig::read() else {
        return BTreeMap::new();
    };
    config
        .contexts
        .into_iter()
        .map(|c| {
            let cluster = c.context.map(|ctx| ctx.cluster).unwrap_or_default();
            (c.name, cluster)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kind(group: &str, plural: &str, kind: &str) -> ResourceType {
        ResourceType {
            api: ApiResource {
                group: group.into(),
                version: "v1".into(),
                api_version: if group.is_empty() {
                    "v1".into()
                } else {
                    format!("{group}/v1")
                },
                kind: kind.into(),
                plural: plural.into(),
            },
            namespaced: true,
        }
    }

    #[test]
    fn aliases_cover_plural_singular_and_short_name() {
        let pods = kind("", "pods", "Pod");
        let aliases = pods.aliases();
        assert!(aliases.contains(&"pods".to_string()));
        assert!(aliases.contains(&"pod".to_string()));
        assert!(aliases.contains(&"po".to_string()));
    }

    #[test]
    fn grouped_kinds_are_qualified() {
        let deploys = kind("apps", "deployments", "Deployment");
        assert_eq!(deploys.qualified(), "deployments.apps");
        assert!(deploys.aliases().contains(&"deploy".to_string()));
    }

    #[test]
    fn resolve_accepts_any_alias() {
        let types = vec![
            kind("", "pods", "Pod"),
            kind("apps", "deployments", "Deployment"),
        ];
        assert_eq!(resolve(&types, "po").unwrap().api.kind, "Pod");
        assert_eq!(
            resolve(&types, "Deployment").unwrap().api.kind,
            "Deployment"
        );
        assert_eq!(
            resolve(&types, "deployments.apps").unwrap().api.kind,
            "Deployment"
        );
        assert!(resolve(&types, "nope").is_none());
    }

    fn pod(containers: serde_json::Value, phase: &str) -> serde_json::Value {
        json!({"status": {"phase": phase, "containerStatuses": containers}})
    }

    #[test]
    fn a_crashlooping_pod_reports_the_reason_not_the_phase() {
        // The whole point: phase is still "Running" here.
        let obj = pod(
            json!([{"ready": false, "restartCount": 7,
                    "state": {"waiting": {"reason": "CrashLoopBackOff"}}}]),
            "Running",
        );
        assert_eq!(summarise(&obj), ("CrashLoopBackOff".to_string(), true));
    }

    #[test]
    fn transient_container_states_are_not_problems() {
        let obj = pod(
            json!([{"ready": false, "state": {"waiting": {"reason": "ContainerCreating"}}}]),
            "Pending",
        );
        assert_eq!(summarise(&obj), ("0/1 Pending".to_string(), true));

        let done = pod(
            json!([{"ready": true, "state": {"terminated": {"reason": "Completed"}}}]),
            "Succeeded",
        );
        assert_eq!(summarise(&done), ("Succeeded".to_string(), false));
    }

    #[test]
    fn a_healthy_pod_still_surfaces_its_restart_count() {
        let obj = pod(
            json!([{"ready": true, "restartCount": 3, "state": {"running": {}}}]),
            "Running",
        );
        assert_eq!(summarise(&obj), ("Running ↺3".to_string(), false));
        let clean = pod(
            json!([{"ready": true, "restartCount": 0, "state": {"running": {}}}]),
            "Running",
        );
        assert_eq!(summarise(&clean), ("Running".to_string(), false));
    }

    #[test]
    fn a_partially_ready_pod_is_unhealthy() {
        let obj = pod(
            json!([{"ready": true, "state": {"running": {}}},
                   {"ready": false, "state": {"running": {}}}]),
            "Running",
        );
        assert_eq!(summarise(&obj), ("1/2 Running".to_string(), true));
    }

    #[test]
    fn summarises_a_phase_when_there_are_no_containers() {
        assert_eq!(
            summarise(&json!({"status": {"phase": "Bound"}})),
            ("Bound".to_string(), false)
        );
        assert_eq!(
            summarise(&json!({"status": {"phase": "Pending"}})),
            ("Pending".to_string(), true)
        );
    }

    #[test]
    fn extracts_service_and_workload_selectors() {
        assert_eq!(
            pod_selector(&json!({"spec": {"selector": {"app": "api"}}})),
            [("app".to_string(), "api".to_string())].into()
        );
        assert_eq!(
            pod_selector(&json!({"spec": {"selector": {"matchLabels": {"app": "api"}}}})),
            [("app".to_string(), "api".to_string())].into()
        );
        assert!(pod_selector(&json!({"spec": {}})).is_empty());
    }

    #[test]
    fn summarises_workload_replicas() {
        let obj = json!({"spec": {"replicas": 3}, "status": {"readyReplicas": 3}});
        assert_eq!(summarise(&obj), ("3/3".to_string(), false));
        let obj = json!({"spec": {"replicas": 3}, "status": {"readyReplicas": 1}});
        assert_eq!(summarise(&obj), ("1/3".to_string(), true));
        // No readyReplicas at all means nothing is ready yet.
        let obj = json!({"spec": {"replicas": 2}, "status": {}});
        assert_eq!(summarise(&obj), ("0/2".to_string(), true));
    }

    #[test]
    fn summarises_a_daemonset() {
        let obj = json!({"status": {"desiredNumberScheduled": 4, "numberReady": 4}});
        assert_eq!(summarise(&obj), ("4/4".to_string(), false));
    }

    #[test]
    fn falls_back_to_conditions_then_to_nothing() {
        let obj = json!({"status": {"conditions": [{"type": "Ready", "status": "False"}]}});
        assert_eq!(summarise(&obj), ("NotReady".to_string(), true));
        assert_eq!(summarise(&json!({})), (String::new(), false));
    }
}
