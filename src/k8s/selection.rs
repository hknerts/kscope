//! Resolving "the thing I have open" into "the pods whose logs I want".
//!
//! This is what makes kscope a workload log viewer rather than a pod log
//! viewer: opening a Deployment streams all of its replicas into one timeline,
//! and a Service streams whatever it currently selects. The rules are pure
//! functions over the pod inventory so they can be tested without a cluster.

use std::collections::BTreeMap;

use super::PodInfo;

/// A parsed `-l`/`--selector` expression.
///
/// Equality-based only — the same subset `kubectl get -l` accepts in practice:
/// `k=v`, `k==v`, `k!=v`, bare `k` (exists) and `!k` (does not exist).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelSelector {
    requirements: Vec<Requirement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Requirement {
    Equals(String, String),
    NotEquals(String, String),
    Exists(String),
    NotExists(String),
}

impl LabelSelector {
    /// Parse a comma-separated selector. An empty string matches everything.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut requirements = Vec::new();
        for term in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            // `!=` must be tested before `=`, and `==` normalises to `=`.
            let requirement = if let Some((k, v)) = term.split_once("!=") {
                Requirement::NotEquals(k.trim().to_string(), v.trim().to_string())
            } else if let Some((k, v)) = term.split_once("==") {
                Requirement::Equals(k.trim().to_string(), v.trim().to_string())
            } else if let Some((k, v)) = term.split_once('=') {
                Requirement::Equals(k.trim().to_string(), v.trim().to_string())
            } else if let Some(k) = term.strip_prefix('!') {
                Requirement::NotExists(k.trim().to_string())
            } else {
                Requirement::Exists(term.to_string())
            };
            if requirement.key().is_empty() {
                return Err(format!("empty label key in {term:?}"));
            }
            requirements.push(requirement);
        }
        Ok(Self { requirements })
    }

    pub fn is_empty(&self) -> bool {
        self.requirements.is_empty()
    }

    /// Every requirement must hold — terms are ANDed, as in Kubernetes.
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.requirements.iter().all(|r| match r {
            Requirement::Equals(k, v) => labels.get(k).is_some_and(|got| got == v),
            // A missing label satisfies `k!=v`, matching Kubernetes semantics.
            Requirement::NotEquals(k, v) => labels.get(k).is_none_or(|got| got != v),
            Requirement::Exists(k) => labels.contains_key(k),
            Requirement::NotExists(k) => !labels.contains_key(k),
        })
    }
}

impl Requirement {
    fn key(&self) -> &str {
        match self {
            Requirement::Equals(k, _)
            | Requirement::NotEquals(k, _)
            | Requirement::Exists(k)
            | Requirement::NotExists(k) => k,
        }
    }
}

/// The object whose logs are wanted.
#[derive(Debug, Clone, Default)]
pub struct LogTarget {
    /// Object kind as the API reports it, e.g. "Deployment". Case-insensitive.
    pub kind: String,
    pub namespace: String,
    pub name: String,
    /// The object's own pod selector, when it has one (Services, and any CRD
    /// that follows the `spec.selector` convention).
    pub selector: BTreeMap<String, String>,
}

/// Workload kinds whose pods carry a controller `ownerReference` back to them.
/// ReplicaSets are rewritten to Deployments during inventory collection, so a
/// Deployment matches its pods directly.
fn owns_pods_directly(kind: &str) -> bool {
    matches!(
        kind,
        "deployment" | "statefulset" | "daemonset" | "job" | "replicaset" | "replicationcontroller"
    )
}

/// Pods whose logs belong to `target`, further narrowed by `selector`.
///
/// Returns them in inventory order, which is namespace-then-name, so the
/// merged timeline is at least deterministic between refreshes.
pub fn pods_for<'a>(
    target: &LogTarget,
    pods: &'a [PodInfo],
    selector: &LabelSelector,
) -> Vec<&'a PodInfo> {
    let kind = target.kind.to_ascii_lowercase();
    pods.iter()
        .filter(|pod| matches_target(&kind, target, pod))
        .filter(|pod| selector.matches(&pod.labels))
        .collect()
}

fn matches_target(kind: &str, target: &LogTarget, pod: &PodInfo) -> bool {
    match kind {
        "pod" => pod.namespace == target.namespace && pod.name == target.name,
        // Cluster-scoped: a node's pods live in every namespace.
        "node" => pod.node == target.name,
        "namespace" => pod.namespace == target.name,
        _ if owns_pods_directly(kind) => {
            pod.namespace == target.namespace
                && pod.owner_kind.to_ascii_lowercase() == kind
                && pod.owner_name == target.name
        }
        // Services — and CRDs shaped like them — select by label. An empty
        // selector must not match every pod in the namespace, so it selects
        // nothing instead.
        _ => {
            !target.selector.is_empty()
                && pod.namespace == target.namespace
                && target
                    .selector
                    .iter()
                    .all(|(k, v)| pod.labels.get(k).is_some_and(|got| got == v))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod(
        ns: &str,
        name: &str,
        owner: (&str, &str),
        node: &str,
        labels: &[(&str, &str)],
    ) -> PodInfo {
        PodInfo {
            namespace: ns.into(),
            name: name.into(),
            node: node.into(),
            owner_kind: owner.0.into(),
            owner_name: owner.1.into(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    fn inventory() -> Vec<PodInfo> {
        vec![
            pod(
                "shop",
                "api-1",
                ("Deployment", "api"),
                "n1",
                &[("app", "api")],
            ),
            pod(
                "shop",
                "api-2",
                ("Deployment", "api"),
                "n2",
                &[("app", "api")],
            ),
            pod(
                "shop",
                "db-0",
                ("StatefulSet", "db"),
                "n1",
                &[("app", "db")],
            ),
            pod(
                "other",
                "api-1",
                ("Deployment", "api"),
                "n1",
                &[("app", "api")],
            ),
            pod("shop", "lonely", ("", ""), "n2", &[]),
        ]
    }

    fn names(pods: Vec<&PodInfo>) -> Vec<String> {
        pods.iter()
            .map(|p| format!("{}/{}", p.namespace, p.name))
            .collect()
    }

    fn target(kind: &str, ns: &str, name: &str) -> LogTarget {
        LogTarget {
            kind: kind.into(),
            namespace: ns.into(),
            name: name.into(),
            selector: BTreeMap::new(),
        }
    }

    #[test]
    fn a_deployment_selects_all_of_its_replicas_in_its_namespace() {
        let inv = inventory();
        let got = pods_for(
            &target("Deployment", "shop", "api"),
            &inv,
            &LabelSelector::default(),
        );
        assert_eq!(names(got), ["shop/api-1", "shop/api-2"]);
    }

    #[test]
    fn a_pod_selects_only_itself() {
        let inv = inventory();
        let got = pods_for(
            &target("Pod", "shop", "api-1"),
            &inv,
            &LabelSelector::default(),
        );
        assert_eq!(names(got), ["shop/api-1"]);
    }

    #[test]
    fn a_node_selects_across_namespaces() {
        let inv = inventory();
        let got = pods_for(&target("Node", "", "n1"), &inv, &LabelSelector::default());
        assert_eq!(names(got), ["shop/api-1", "shop/db-0", "other/api-1"]);
    }

    #[test]
    fn a_service_selects_by_its_own_label_selector() {
        let mut t = target("Service", "shop", "api-svc");
        t.selector = [("app".to_string(), "api".to_string())].into();
        let inv = inventory();
        assert_eq!(
            names(pods_for(&t, &inv, &LabelSelector::default())),
            ["shop/api-1", "shop/api-2"]
        );
    }

    #[test]
    fn a_selectorless_object_selects_nothing_rather_than_everything() {
        // The dangerous default: an empty selector must not sweep in the whole
        // namespace and attach a stream to every pod in it.
        let inv = inventory();
        let got = pods_for(
            &target("Service", "shop", "headless"),
            &inv,
            &LabelSelector::default(),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn an_extra_selector_narrows_the_result() {
        let sel = LabelSelector::parse("app=db").unwrap();
        let inv = inventory();
        let got = pods_for(&target("Namespace", "", "shop"), &inv, &sel);
        assert_eq!(names(got), ["shop/db-0"]);
    }

    #[test]
    fn parses_every_supported_operator() {
        let labels: BTreeMap<String, String> = [
            ("app".to_string(), "api".to_string()),
            ("tier".to_string(), "web".to_string()),
        ]
        .into();
        assert!(LabelSelector::parse("app=api").unwrap().matches(&labels));
        assert!(LabelSelector::parse("app==api").unwrap().matches(&labels));
        assert!(LabelSelector::parse("app!=db").unwrap().matches(&labels));
        assert!(LabelSelector::parse("app").unwrap().matches(&labels));
        assert!(LabelSelector::parse("!missing").unwrap().matches(&labels));
        assert!(LabelSelector::parse("app=api,tier=web")
            .unwrap()
            .matches(&labels));
        assert!(!LabelSelector::parse("app=api,tier=db")
            .unwrap()
            .matches(&labels));
        assert!(!LabelSelector::parse("!app").unwrap().matches(&labels));
    }

    #[test]
    fn a_missing_label_satisfies_not_equals() {
        // Kubernetes semantics: absent counts as "not that value".
        let labels = BTreeMap::new();
        assert!(LabelSelector::parse("app!=api").unwrap().matches(&labels));
    }

    #[test]
    fn an_empty_selector_matches_everything() {
        assert!(LabelSelector::parse("  ").unwrap().is_empty());
        assert!(LabelSelector::parse("").unwrap().matches(&BTreeMap::new()));
    }

    #[test]
    fn rejects_an_empty_key() {
        assert!(LabelSelector::parse("=value").is_err());
    }
}
