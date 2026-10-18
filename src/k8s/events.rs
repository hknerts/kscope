//! Periodic cluster-event refresh.
//!
//! Events are their own poller rather than part of [`super::discovery`]: they
//! churn far faster than the pod inventory, and a namespace-scoped token that
//! cannot list them must not take the rest of the inventory down with it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use k8s_openapi::api::core::v1::Event;
use kube::api::Api;
use kube::runtime::reflector;
use kube::runtime::watcher::{self, watcher};
use kube::runtime::WatchStreamExt;
use kube::ResourceExt;
use tokio::sync::mpsc::Sender;

use super::discovery::Scope;
use super::watch::{drive, Flags};

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
        // Called for every event on every cursor move, so no allocation.
        let selector = selector.trim();
        if selector.is_empty() {
            return true;
        }
        if self.kind.is_empty() {
            return self.name.eq_ignore_ascii_case(selector);
        }
        match selector.split_once('/') {
            Some((kind, name)) => {
                self.kind.eq_ignore_ascii_case(kind) && self.name.eq_ignore_ascii_case(name)
            }
            None => self.kind.eq_ignore_ascii_case(selector),
        }
    }
}

/// Message sent from the event poller to the UI.
#[derive(Debug)]
pub enum EventUpdate {
    Events(Vec<EventInfo>),
    /// Listing failed — usually RBAC. Shown in the events pane, not fatal.
    Unavailable(String),
}

/// Spawn the event watch. It keeps running until the channel is closed or the
/// handle is aborted.
///
/// Events are watched rather than re-listed: they are the fastest-churning
/// kind in any cluster, so a periodic LIST of up to thousands of them was
/// mostly re-downloading what kscope already had. `heartbeat` is how often the
/// list is re-sent when nothing changed, so ages keep moving.
pub fn spawn(
    client: kube::Client,
    scope: Scope,
    heartbeat: Duration,
    tx: Sender<EventUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let api: Api<Event> = match &scope {
            Scope::AllNamespaces => Api::all(client),
            Scope::Namespace(ns) => Api::namespaced(client, ns),
        };
        let (store, writer) = reflector::store::<Event>();
        let flags = Flags::default();
        let changed = Arc::new(AtomicBool::new(true));

        let err_tx = tx.clone();
        let mut last_error = String::new();
        let watch = drive(
            writer,
            watcher(api, watcher::Config::default()).modify(|e| {
                e.managed_fields_mut().clear();
            }),
            flags.clone(),
            changed.clone(),
            move |err: String| {
                // Usually RBAC; say it once, not on every retry.
                if err != last_error {
                    last_error = err.clone();
                    let _ = err_tx.try_send(EventUpdate::Unavailable(format!("events: {err}")));
                }
            },
        );

        let publish = async {
            let mut last_sent: Option<Instant> = None;
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if !flags.is_ready() {
                    continue;
                }
                let due = last_sent.is_none_or(|t| t.elapsed() >= heartbeat);
                if !changed.swap(false, Ordering::AcqRel) && !due {
                    continue;
                }
                let events = collect(&store.state());
                if tx.send(EventUpdate::Events(events)).await.is_err() {
                    return;
                }
                last_sent = Some(Instant::now());
            }
        };

        tokio::select! {
            _ = publish => {}
            _ = watch => {}
        }
    })
}

/// Flatten the cached events, newest first.
pub fn collect(objects: &[Arc<Event>]) -> Vec<EventInfo> {
    let now = chrono::Utc::now().timestamp();
    let mut events: Vec<EventInfo> = objects.iter().map(|e| convert(e, now)).collect();
    // Most recent first: that is the only order anyone reads events in.
    events.sort_by_key(|e| e.age_seconds);
    events
}

fn convert(event: &Event, now: i64) -> EventInfo {
    let obj = &event.involved_object;

    // `last_timestamp` is the classic field; the newer `event_time` is all a
    // series-style event carries, so fall back through both before giving up
    // and using the object's own creation time.
    let last_seen = event
        .last_timestamp
        .as_ref()
        .map(|t| t.0.as_second())
        .or_else(|| event.event_time.as_ref().map(|t| t.0.as_second()))
        .or_else(|| event.first_timestamp.as_ref().map(|t| t.0.as_second()))
        .or_else(|| {
            event
                .metadata
                .creation_timestamp
                .as_ref()
                .map(|t| t.0.as_second())
        });

    let source = event
        .source
        .as_ref()
        .and_then(|s| s.component.clone())
        .or_else(|| event.reporting_component.clone())
        .unwrap_or_default();

    EventInfo {
        namespace: obj
            .namespace
            .clone()
            .or_else(|| event.metadata.namespace.clone())
            .unwrap_or_default(),
        kind: obj.kind.clone().unwrap_or_default(),
        name: obj.name.clone().unwrap_or_default(),
        reason: event.reason.clone().unwrap_or_default(),
        // Events routinely carry embedded newlines; the table wants one line.
        message: event
            .message
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        type_: event.type_.clone().unwrap_or_else(|| "Normal".into()),
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
