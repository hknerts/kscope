//! The one piece of plumbing every watched kind shares: run a reflector,
//! and tell a publisher when its cache has something new to say.

use std::fmt::Debug;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::{Stream, StreamExt};
use kube::runtime::reflector::{self, Lookup};
use kube::runtime::watcher::{self, Event};
use kube::runtime::WatchStreamExt;

/// Per-watch state shared with the publisher.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    /// The initial LIST has completed at least once.
    ready: Arc<AtomicBool>,
}

impl Flags {
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

/// Run one reflector until it is dropped, flagging every change.
pub async fn drive<K, S>(
    writer: reflector::store::Writer<K>,
    stream: S,
    flags: Flags,
    changed: Arc<AtomicBool>,
    mut on_error: impl FnMut(String),
) where
    K: Lookup + Clone + Debug + Send + Sync + 'static,
    K::DynamicType: Eq + Hash + Clone + Default,
    S: Stream<Item = Result<Event<K>, watcher::Error>> + Send,
{
    let stream = reflector::reflector(writer, stream.default_backoff());
    futures::pin_mut!(stream);
    while let Some(event) = stream.next().await {
        match event {
            Ok(Event::InitDone) => {
                flags.ready.store(true, Ordering::Release);
                changed.store(true, Ordering::Release);
            }
            // Mid-relist objects are not a complete picture yet.
            Ok(Event::Init | Event::InitApply(_)) => {}
            Ok(Event::Apply(_) | Event::Delete(_)) => changed.store(true, Ordering::Release),
            Err(err) => on_error(err.to_string()),
        }
    }
}
