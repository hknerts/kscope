//! Fleets: several kubeconfig contexts listed as one.
//!
//! An environment is rarely one cluster — "staging" may be five of them. A
//! fleet names a set of contexts (by glob, from the config file or the
//! command line) and lists a kind across all of them at once, every row
//! tagged with the context it came from. Members are queried concurrently
//! and independently: an unreachable cluster is reported, never fatal, and
//! never holds the others back for longer than [`MEMBER_TIMEOUT`].

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::discovery::Scope;
use super::resources::{ResourceRow, ResourceType};

/// How long one member may take to answer before it is reported unreachable.
pub const MEMBER_TIMEOUT: Duration = Duration::from_secs(10);

/// A named set of contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fleet {
    pub name: String,
    pub members: Vec<String>,
}

impl Fleet {
    /// Resolve `spec` against the configured fleets and the kubeconfig's
    /// contexts. `spec` is a configured fleet name, or a comma-separated list
    /// of context globs (`stg-*,qa-eu`). `None` when nothing matches.
    pub fn resolve(
        spec: &str,
        configured: &BTreeMap<String, Vec<String>>,
        contexts: &[String],
    ) -> Option<Fleet> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        let patterns: Vec<String> = match configured.get(spec) {
            Some(patterns) => patterns.clone(),
            None => spec.split(',').map(|p| p.trim().to_string()).collect(),
        };
        let members: Vec<String> = contexts
            .iter()
            .filter(|c| patterns.iter().any(|p| glob_match(p, c)))
            .cloned()
            .collect();
        (!members.is_empty()).then(|| Fleet {
            name: spec.to_string(),
            members,
        })
    }

    /// Every configured fleet that matches at least one context, in name
    /// order — what the contexts pane offers.
    pub fn all_configured(
        configured: &BTreeMap<String, Vec<String>>,
        contexts: &[String],
    ) -> Vec<Fleet> {
        configured
            .keys()
            .filter_map(|name| Fleet::resolve(name, configured, contexts))
            .collect()
    }
}

/// Shell-style glob: `*` is any run of characters, `?` any one character.
/// Anything else matches itself, case-sensitively, like context names do.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Where the last `*` was, and how much of the text it has swallowed.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Clients for fleet members, built on first use and kept: building one reads
/// the kubeconfig and may run an exec credential plugin, which is far too slow
/// to repeat on every refresh.
#[derive(Clone, Default)]
pub struct Clients {
    inner: Arc<Mutex<HashMap<String, kube::Client>>>,
}

impl Clients {
    /// Seed the cache with a client that already exists (the active context).
    pub async fn insert(&self, context: &str, client: kube::Client) {
        self.inner.lock().await.insert(context.to_string(), client);
    }

    /// The client for `context`, building it on first use.
    pub async fn get(&self, context: &str) -> anyhow::Result<kube::Client> {
        if let Some(client) = self.inner.lock().await.get(context) {
            return Ok(client.clone());
        }
        let (client, _, _, _) = super::connect(Some(context)).await?;
        self.inner
            .lock()
            .await
            .insert(context.to_string(), client.clone());
        Ok(client)
    }
}

/// The rows of one kind across a fleet, plus the members that did not answer.
#[derive(Debug, Default)]
pub struct Listing {
    pub rows: Vec<ResourceRow>,
    /// `(context, error)` for every member that failed or timed out.
    pub failed: Vec<(String, String)>,
}

/// The member to connect to first: `preferred` (the kubeconfig's current
/// context) when it is a member and answers, else whichever member answers
/// first. Connecting to a dead member would leave discovery — and with it
/// the whole fleet listing — waiting on a connect timeout.
pub async fn first_reachable(
    clients: &Clients,
    members: &[String],
    preferred: Option<&str>,
) -> Option<String> {
    let probe = |context: String| async move {
        let answered = tokio::time::timeout(MEMBER_TIMEOUT, async {
            let client = clients.get(&context).await?;
            client.apiserver_version().await?;
            anyhow::Ok(())
        })
        .await;
        match answered {
            Ok(Ok(())) => Ok(context),
            _ => Err(()),
        }
    };
    if let Some(preferred) = preferred.filter(|p| members.iter().any(|m| m == p)) {
        if let Ok(context) = probe(preferred.to_string()).await {
            return Some(context);
        }
    }
    let probes = members.iter().map(|m| Box::pin(probe(m.clone())));
    futures::future::select_ok(probes)
        .await
        .ok()
        .map(|(context, _)| context)
}

/// List `kind` in `scope` on every member at once.
pub async fn list(
    clients: &Clients,
    members: &[String],
    kind: &ResourceType,
    scope: &Scope,
    selector: &str,
) -> Listing {
    let tasks = members.iter().map(|context| async move {
        let result = tokio::time::timeout(MEMBER_TIMEOUT, async {
            let client = clients.get(context).await?;
            super::resources::list(&client, kind, scope, selector).await
        })
        .await;
        let result = match result {
            Ok(Ok(rows)) => Ok(rows),
            Ok(Err(err)) => Err(format!("{err:#}")),
            Err(_) => Err(format!("no answer in {}s", MEMBER_TIMEOUT.as_secs())),
        };
        (context.clone(), result)
    });

    let mut listing = Listing::default();
    for (context, result) in futures::future::join_all(tasks).await {
        match result {
            Ok(rows) => listing.rows.extend(rows.into_iter().map(|mut row| {
                row.context = context.clone();
                row
            })),
            Err(err) => listing.failed.push((context, err)),
        }
    }
    // Members were queried in config order; present one stable table.
    listing.rows.sort_by(|a, b| {
        (&a.context, &a.namespace, &a.name).cmp(&(&b.context, &b.namespace, &b.name))
    });
    listing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contexts() -> Vec<String> {
        [
            "kind-stg-eu",
            "kind-stg-us",
            "kind-prod-eu",
            "kind-prod-us",
            "minikube",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn globs() {
        assert!(glob_match("kind-stg-*", "kind-stg-eu"));
        assert!(glob_match("*stg*", "kind-stg-eu"));
        assert!(glob_match("kind-???-eu", "kind-stg-eu"));
        assert!(glob_match("*", ""));
        assert!(glob_match("minikube", "minikube"));
        assert!(!glob_match("kind-stg-*", "kind-prod-eu"));
        assert!(!glob_match("stg", "kind-stg-eu"));
        assert!(!glob_match("kind-?", "kind-"));
        // Backtracking: the first `-` is not the one that has to match.
        assert!(glob_match("*-eu", "kind-stg-eu"));
        assert!(glob_match("a*b*c", "aXbYbZc"));
    }

    #[test]
    fn resolves_configured_fleets_by_name() {
        let configured = BTreeMap::from([("staging".to_string(), vec!["kind-stg-*".to_string()])]);
        let fleet = Fleet::resolve("staging", &configured, &contexts()).unwrap();
        assert_eq!(fleet.name, "staging");
        assert_eq!(fleet.members, vec!["kind-stg-eu", "kind-stg-us"]);
    }

    #[test]
    fn resolves_ad_hoc_globs() {
        let none = BTreeMap::new();
        let fleet = Fleet::resolve("*-eu, minikube", &none, &contexts()).unwrap();
        // Kubeconfig order, not pattern order, and no duplicates.
        assert_eq!(
            fleet.members,
            vec!["kind-stg-eu", "kind-prod-eu", "minikube"]
        );
        assert!(Fleet::resolve("nothing-*", &none, &contexts()).is_none());
        assert!(Fleet::resolve("  ", &none, &contexts()).is_none());
    }

    #[test]
    fn lists_only_fleets_with_members() {
        let configured = BTreeMap::from([
            ("prod".to_string(), vec!["kind-prod-*".to_string()]),
            ("qa".to_string(), vec!["qa-*".to_string()]),
        ]);
        let fleets = Fleet::all_configured(&configured, &contexts());
        assert_eq!(fleets.len(), 1);
        assert_eq!(fleets[0].name, "prod");
    }
}
