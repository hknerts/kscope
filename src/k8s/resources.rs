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
    /// True when `status` reads as a problem, so the table can colour it.
    pub unhealthy: bool,
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
) -> Result<Vec<ResourceRow>> {
    let api: Api<DynamicObject> = match (kind.namespaced, scope) {
        (false, _) | (true, Scope::AllNamespaces) => Api::all_with(client.clone(), &kind.api),
        (true, Scope::Namespace(ns)) => Api::namespaced_with(client.clone(), ns, &kind.api),
    };
    let list = api.list(&ListParams::default().limit(1000)).await?;
    let now = chrono::Utc::now().timestamp();

    let mut rows: Vec<ResourceRow> = list.items.iter().map(|o| convert(o, now)).collect();
    rows.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));
    Ok(rows)
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
    }
}

/// A one-word health summary from whatever shape the object happens to have.
/// Deliberately generic: it works on CRDs that follow the usual conventions
/// and falls back to an empty string rather than guessing wrong.
fn summarise(data: &serde_json::Value) -> (String, bool) {
    let status = &data["status"];

    // Pods and PVCs: a phase is the whole story.
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

    #[test]
    fn summarises_a_pod_phase() {
        let (status, bad) = summarise(&json!({"status": {"phase": "Running"}}));
        assert_eq!(status, "Running");
        assert!(!bad);
        let (status, bad) = summarise(&json!({"status": {"phase": "CrashLoopBackOff"}}));
        assert_eq!(status, "CrashLoopBackOff");
        assert!(bad);
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
