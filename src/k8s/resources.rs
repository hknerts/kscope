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
///
/// Aggregated discovery (Kubernetes 1.26+) answers in two requests; the
/// classic walk costs one request per API group, sequentially, which on a
/// cluster with a few dozen CRD groups was most of kscope's startup time.
pub async fn discover(client: &kube::Client) -> Result<Vec<ResourceType>> {
    let discovery = match Discovery::new(client.clone()).run_aggregated().await {
        Ok(discovery) => discovery,
        Err(_) => Discovery::new(client.clone()).run().await?,
    };
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
    /// The context the row was listed from, when browsing a fleet. Empty for
    /// an ordinary single-cluster listing.
    pub context: String,
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
    /// Container images, for the kinds that run containers: a pod's own,
    /// a workload's pod template's, a CronJob's job template's.
    pub images: Vec<String>,
    /// Current CPU (millicores) and memory (bytes): the pod's own, or the
    /// sum over the pods the object selects. `None` when metrics-server is
    /// not installed or no running pod reports usage.
    pub usage: Option<(f64, f64)>,
}

impl ResourceRow {
    /// Pods, workloads and Services: kinds whose rows can carry usage.
    pub fn runs_pods(&self) -> bool {
        !self.images.is_empty() || !self.selector.is_empty()
    }
    pub fn key(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.namespace, self.name)
        }
    }

    /// `context/namespace/name`, as `kscope get -o name` prints it. The
    /// namespace segment stays (empty) for cluster-scoped objects, so the
    /// name is always the third `/`-separated field.
    pub fn qualified_name(&self) -> String {
        format!("{}/{}/{}", self.context, self.namespace, self.name)
    }

    /// The row as `kscope get -o json` prints it. A stable, small shape for
    /// scripts; the table's derived columns (usage, images) are left out.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "context": self.context,
            "namespace": self.namespace,
            "name": self.name,
            "status": self.status,
            "age_seconds": self.age_seconds,
        })
    }

    /// Unique across a fleet: the same `namespace/name` can exist in every
    /// member, and keeping the selection must not jump between them.
    pub fn identity(&self) -> String {
        if self.context.is_empty() {
            self.key()
        } else {
            format!("{}:{}", self.context, self.key())
        }
    }
}

/// Objects per LIST page.
const PAGE: u32 = 500;
/// Hard ceiling on a listing, so a runaway kind cannot exhaust memory.
const MAX_ROWS: usize = 50_000;

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
    let mut lp = ListParams::default().limit(PAGE);
    // Let the API server do the filtering when a selector is set.
    if !selector.trim().is_empty() {
        lp = lp.labels(selector.trim());
    }
    let now = chrono::Utc::now().timestamp();

    // Page through rather than stopping at the first page: a single capped
    // LIST silently hid everything past item 1000 on a big cluster. Each page
    // is converted and dropped straight away, so the full objects are never
    // all resident at once.
    let mut rows: Vec<ResourceRow> = Vec::new();
    loop {
        let page = api.list(&lp).await?;
        rows.extend(page.items.iter().map(|o| convert(o, now)));
        match page.metadata.continue_.filter(|c| !c.is_empty()) {
            Some(token) if rows.len() < MAX_ROWS => lp = lp.continue_token(&token),
            _ => break,
        }
    }
    rows.sort_by(|a, b| a.namespace.cmp(&b.namespace).then(a.name.cmp(&b.name)));

    // Usage is decoration: a cluster without metrics-server, or one slow to
    // answer, gets "-" in those columns rather than a slower list.
    if rows.iter().any(ResourceRow::runs_pods) {
        let usage =
            tokio::time::timeout(USAGE_TIMEOUT, super::metrics::pod_usage(client, scope)).await;
        if let Ok(Ok(samples)) = usage {
            attach_usage(&mut rows, &kind.api.kind, &samples);
        }
    }
    Ok(rows)
}

/// How long the list waits for metrics-server before leaving usage out.
const USAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Give every row its usage: a Pod its own, anything with a pod selector
/// the sum over the pods it selects in its namespace.
pub fn attach_usage(rows: &mut [ResourceRow], kind: &str, samples: &[super::metrics::PodSample]) {
    let by_name: std::collections::HashMap<(&str, &str), &super::metrics::PodSample> = samples
        .iter()
        .map(|s| ((s.namespace.as_str(), s.name.as_str()), s))
        .collect();
    for row in rows.iter_mut() {
        let selected: Vec<&super::metrics::PodSample> = if kind == "Pod" {
            by_name
                .get(&(row.namespace.as_str(), row.name.as_str()))
                .into_iter()
                .copied()
                .collect()
        } else if row.selector.is_empty() {
            continue;
        } else {
            samples
                .iter()
                .filter(|s| {
                    s.namespace == row.namespace
                        && row
                            .selector
                            .iter()
                            .all(|(k, v)| s.labels.get(k).is_some_and(|got| got == v))
                })
                .collect()
        };
        if !selected.is_empty() {
            row.usage = Some((
                selected.iter().map(|s| s.cpu_milli()).sum(),
                selected.iter().map(|s| s.mem_bytes()).sum(),
            ));
        }
    }
}

/// `250m 128.0Mi`, or `-` for each when there is no usage.
pub fn fmt_usage(usage: Option<(f64, f64)>) -> (String, String) {
    match usage {
        Some((cpu, mem)) => (crate::metrics::fmt_cpu(cpu), crate::metrics::fmt_bytes(mem)),
        None => ("-".into(), "-".into()),
    }
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
        context: String::new(),
        namespace: obj.metadata.namespace.clone().unwrap_or_default(),
        name: obj.metadata.name.clone().unwrap_or_default(),
        age_seconds,
        status,
        unhealthy,
        selector: pod_selector(&obj.data),
        images: container_images(&obj.data),
        usage: None,
    }
}

/// The images an object runs, in spec order, init containers left out.
/// Containers live at a different depth per kind: a Pod's spec, a workload's
/// pod template, a CronJob's job template.
pub fn container_images(data: &serde_json::Value) -> Vec<String> {
    let pod_specs = [
        &data["spec"],
        &data["spec"]["template"]["spec"],
        &data["spec"]["jobTemplate"]["spec"]["template"]["spec"],
    ];
    pod_specs
        .iter()
        .flat_map(|spec| spec["containers"].as_array().into_iter().flatten())
        .filter_map(|c| c["image"].as_str().map(str::to_string))
        .collect()
}

/// An image as name and version: `registry.k8s.io/coredns/coredns:v1.14.2`
/// → `coredns:v1.14.2`. The registry and path are the same long prefix on
/// every row and say little; the version is what differs. No tag means
/// `latest`, and is shown as such. A digest is cut to 12 characters, enough
/// to tell two apart.
pub fn short_image(image: &str) -> String {
    let (path, digest) = match image.split_once('@') {
        Some((path, digest)) => (path, Some(digest)),
        None => (image, None),
    };
    // The last path segment is the name; a `:` in it starts the tag (a `:`
    // before it is a registry port).
    let last = path.rsplit('/').next().unwrap_or(path);
    let (name, tag) = match last.split_once(':') {
        Some((name, tag)) => (name, Some(tag)),
        None => (last, None),
    };
    match (tag, digest) {
        (Some(tag), _) => format!("{name}:{tag}"),
        (None, Some(digest)) => {
            let hash = digest.strip_prefix("sha256:").unwrap_or(digest);
            format!("{name}@{}", &hash[..hash.len().min(12)])
        }
        (None, None) => format!("{name}:latest"),
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

    #[test]
    fn rows_print_as_json_and_qualified_names() {
        let pod = ResourceRow {
            context: "stg-eu".into(),
            namespace: "shop".into(),
            name: "api-0".into(),
            status: "Running".into(),
            age_seconds: 90,
            images: vec!["busybox:1.36".into()],
            ..Default::default()
        };
        assert_eq!(pod.qualified_name(), "stg-eu/shop/api-0");
        assert_eq!(
            pod.to_json(),
            serde_json::json!({
                "context": "stg-eu",
                "namespace": "shop",
                "name": "api-0",
                "status": "Running",
                "age_seconds": 90,
            })
        );
        let node = ResourceRow {
            context: "stg-eu".into(),
            name: "worker".into(),
            ..Default::default()
        };
        assert_eq!(node.qualified_name(), "stg-eu//worker");
        assert_eq!(node.to_json()["namespace"], "");
    }

    #[test]
    fn usage_is_a_pods_own_or_the_sum_of_what_a_workload_selects() {
        use super::super::metrics::{ContainerSample, PodSample};
        let sample = |name: &str, app: &str, cpu: f64| PodSample {
            namespace: "shop".into(),
            name: name.into(),
            labels: [("app".to_string(), app.to_string())].into(),
            containers: vec![ContainerSample {
                name: "c".into(),
                cpu_milli: cpu,
                mem_bytes: 1024.0,
            }],
        };
        let samples = vec![
            sample("api-1", "api", 10.0),
            sample("api-2", "api", 5.0),
            sample("db-0", "db", 1.0),
        ];

        let mut pods = vec![
            ResourceRow {
                namespace: "shop".into(),
                name: "api-2".into(),
                ..Default::default()
            },
            ResourceRow {
                namespace: "shop".into(),
                name: "gone".into(),
                ..Default::default()
            },
        ];
        attach_usage(&mut pods, "Pod", &samples);
        assert_eq!(pods[0].usage, Some((5.0, 1024.0)));
        assert_eq!(pods[1].usage, None);

        let mut deploys = vec![
            ResourceRow {
                namespace: "shop".into(),
                name: "api".into(),
                selector: [("app".to_string(), "api".to_string())].into(),
                ..Default::default()
            },
            // Same labels, other namespace: selects nothing.
            ResourceRow {
                namespace: "other".into(),
                name: "api".into(),
                selector: [("app".to_string(), "api".to_string())].into(),
                ..Default::default()
            },
        ];
        attach_usage(&mut deploys, "Deployment", &samples);
        assert_eq!(deploys[0].usage, Some((15.0, 2048.0)));
        assert_eq!(deploys[1].usage, None);
        assert_eq!(
            fmt_usage(deploys[1].usage),
            ("-".to_string(), "-".to_string())
        );
        assert_eq!(
            fmt_usage(deploys[0].usage),
            ("15m".to_string(), "2.0Ki".to_string())
        );
    }

    #[test]
    fn images_show_name_and_version_only() {
        assert_eq!(
            short_image("registry.k8s.io/coredns/coredns:v1.14.2"),
            "coredns:v1.14.2"
        );
        assert_eq!(short_image("ghcr.io/hknerts/kscope:1.6.2"), "kscope:1.6.2");
        assert_eq!(
            short_image("docker.io/library/busybox:1.36"),
            "busybox:1.36"
        );
        assert_eq!(short_image("busybox:1.36"), "busybox:1.36");
        // The version is always there: no tag is `latest`.
        assert_eq!(short_image("busybox"), "busybox:latest");
        // A registry port is not a tag.
        assert_eq!(short_image("registry.local:5000/team/api"), "api:latest");
        assert_eq!(short_image("registry.local:5000/team/api:2.1"), "api:2.1");
        // A tag wins over a digest; a digest alone is shortened.
        assert_eq!(
            short_image("api:2.1@sha256:0123456789abcdef0123"),
            "api:2.1"
        );
        assert_eq!(
            short_image("123.dkr.ecr.eu-west-1.amazonaws.com/api@sha256:0123456789abcdef0123"),
            "api@0123456789ab"
        );
    }

    #[test]
    fn rows_carry_the_images_of_any_kind_that_runs_containers() {
        let pod = serde_json::json!({"spec": {
            "initContainers": [{"image": "migrate:1"}],
            "containers": [{"image": "api:2"}, {"image": "envoy:1.30"}]
        }});
        assert_eq!(container_images(&pod), vec!["api:2", "envoy:1.30"]);
        let deploy = serde_json::json!({"spec": {"template": {"spec": {"containers": [{"image": "api:2"}]}}}});
        assert_eq!(container_images(&deploy), vec!["api:2"]);
        let svc = serde_json::json!({"spec": {"selector": {"app": "api"}}});
        assert!(container_images(&svc).is_empty());
    }
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
