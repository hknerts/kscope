//! Periodic cluster-event refresh.
//!
//! Events are their own poller rather than part of [`super::discovery`]: they
//! churn far faster than the pod inventory, and a namespace-scoped token that
//! cannot list them must not take the rest of the inventory down with it.

use std::time::Duration;

use anyhow::Result;
use k8s_openapi::api::core::v1::Event;
use kube::api::{Api, ListParams};
use tokio::sync::mpsc::Sender;

use super::discovery::Scope;

/// One cluster event, flattened for display.
#[derive(Debug, Clone, Default)]
pub struct EventInfo {
    pub namespace: String,
    /// Kind of the object the event is about ("Pod", "Deployment", "Node", ...).
    pub kind: String,
    /// Name of the object the event is about.
    pub name: String,
    pub reason: String,
    pub message: String,
    /// "Normal" or "Warning".
    pub type_: String,
    pub count: i32,
    /// Seconds since the event was last observed.
    pub age_seconds: i64,
    /// Emitting controller, e.g. "kubelet" or "deployment-controller".
    pub source: String,
}

impl EventInfo {
    pub fn is_warning(&self) -> bool {
        self.type_ == "Warning"
    }

    /// `kind/name` — what the `:` palette completes against.
    pub fn resource(&self) -> String {
        if self.kind.is_empty() {
            self.name.clone()
        } else {
            format!("{}/{}", self.kind.to_ascii_lowercase(), self.name)
        }
    }

    /// Does this event belong to `selector`, as typed in the `:` palette?
    ///
    /// A bare kind ("pod") matches every object of that kind; a full
    /// "kind/name" matches only that object. `selector` is lowercased here so
    /// callers can pass raw user input.
    pub fn matches_resource(&self, selector: &str) -> bool {
        let selector = selector.trim().to_ascii_lowercase();
        if selector.is_empty() {
            return true;
        }
        self.resource() == selector || self.kind.to_ascii_lowercase() == selector
    }
}

/// Message sent from the event poller to the UI.
#[derive(Debug)]
pub enum EventUpdate {
    Events(Vec<EventInfo>),
    /// Listing failed — usually RBAC. Shown in the events pane, not fatal.
    Unavailable(String),
}

/// Spawn the event loop. It keeps running until the channel is closed.
pub fn spawn(
    client: kube::Client,
    scope: Scope,
    interval: Duration,
    tx: Sender<EventUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let update = match collect(&client, &scope).await {
                Ok(events) => EventUpdate::Events(events),
                Err(err) => EventUpdate::Unavailable(format!("events: {err}")),
            };
            if tx.send(update).await.is_err() {
                return;
            }
        }
    })
}

/// One event pass, newest first.
pub async fn collect(client: &kube::Client, scope: &Scope) -> Result<Vec<EventInfo>> {
    let api: Api<Event> = match scope {
        Scope::AllNamespaces => Api::all(client.clone()),
        Scope::Namespace(ns) => Api::namespaced(client.clone(), ns),
    };
    let list = api.list(&ListParams::default().limit(2000)).await?;
    let now = chrono::Utc::now().timestamp();

    let mut events: Vec<EventInfo> = list.items.into_iter().map(|e| convert(e, now)).collect();
    // Most recent first: that is the only order anyone reads events in.
    events.sort_by_key(|e| e.age_seconds);
    Ok(events)
}

fn convert(event: Event, now: i64) -> EventInfo {
    let obj = event.involved_object;

    // `last_timestamp` is the classic field; the newer `event_time` is all a
    // series-style event carries, so fall back through both before giving up
    // and using the object's own creation time.
    let last_seen = event
        .last_timestamp
        .map(|t| t.0.as_second())
        .or_else(|| event.event_time.map(|t| t.0.as_second()))
        .or_else(|| event.first_timestamp.map(|t| t.0.as_second()))
        .or_else(|| event.metadata.creation_timestamp.map(|t| t.0.as_second()));

    let source = event
        .source
        .and_then(|s| s.component)
        .or(event.reporting_component)
        .unwrap_or_default();

    EventInfo {
        namespace: obj
            .namespace
            .or(event.metadata.namespace)
            .unwrap_or_default(),
        kind: obj.kind.unwrap_or_default(),
        name: obj.name.unwrap_or_default(),
        reason: event.reason.unwrap_or_default(),
        // Events routinely carry embedded newlines; the table wants one line.
        message: event
            .message
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        type_: event.type_.unwrap_or_else(|| "Normal".into()),
        count: event.count.unwrap_or(1).max(1),
        age_seconds: last_seen.map(|t| (now - t).max(0)).unwrap_or(0),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_is_lowercase_kind_slash_name() {
        let e = EventInfo {
            kind: "Pod".into(),
            name: "api-7d9".into(),
            ..Default::default()
        };
        assert_eq!(e.resource(), "pod/api-7d9");
    }

    #[test]
    fn resource_without_kind_is_just_the_name() {
        let e = EventInfo {
            name: "orphan".into(),
            ..Default::default()
        };
        assert_eq!(e.resource(), "orphan");
    }

    #[test]
    fn a_bare_kind_matches_every_object_of_that_kind() {
        let e = EventInfo {
            kind: "Pod".into(),
            name: "api-7d9".into(),
            ..Default::default()
        };
        assert!(e.matches_resource("pod"));
        assert!(e.matches_resource("Pod"));
        assert!(e.matches_resource("pod/api-7d9"));
        assert!(!e.matches_resource("pod/other"));
        assert!(!e.matches_resource("node"));
    }

    #[test]
    fn a_name_prefix_is_not_a_match() {
        // "pod/api" must not sweep in "pod/api-7d9" — the palette always
        // hands over a complete selector, so a partial one means "no".
        let e = EventInfo {
            kind: "Pod".into(),
            name: "api-7d9".into(),
            ..Default::default()
        };
        assert!(!e.matches_resource("pod/api"));
    }

    #[test]
    fn an_empty_selector_matches_everything() {
        assert!(EventInfo::default().matches_resource("   "));
    }
}
