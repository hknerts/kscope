//! Log streaming.
//!
//! By default kscope requests the container's whole retained history — every
//! line the kubelet still has, starting from container start — and then follows.
//! `LogParams::tail_lines = None` is what asks for that; a `Some(n)` only ever
//! narrows it.
//!
//! One task per attached container. Lines are **batched** before being sent to
//! the UI (up to 512 lines or 100 ms, whichever comes first): a chatty pod can
//! emit tens of thousands of lines per second and waking the render loop for
//! each one would be pure overhead.

use std::sync::Arc;
use std::time::Duration;

use futures::{AsyncBufReadExt, TryStreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, LogParams};
use tokio::sync::mpsc::Sender;

const BATCH_MAX: usize = 512;
const BATCH_INTERVAL: Duration = Duration::from_millis(100);

/// What to stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSpec {
    pub namespace: String,
    pub pod: String,
    /// `None` streams the pod's default container.
    pub container: Option<String>,
    /// Number of trailing lines to request. `None` asks the API server for the
    /// container's **entire** retained history, from the moment it started.
    pub tail: Option<i64>,
    /// Only return lines newer than this many seconds, if set.
    pub since_seconds: Option<i64>,
    pub timestamps: bool,
    /// Read the logs of the previous, crashed instance.
    pub previous: bool,
}

impl StreamSpec {
    /// Stable identifier used as the per-line source label.
    pub fn source(&self) -> Arc<str> {
        match &self.container {
            Some(c) => Arc::from(format!("{}/{}:{}", self.namespace, self.pod, c)),
            None => Arc::from(format!("{}/{}", self.namespace, self.pod)),
        }
    }
}

/// Events emitted by a streaming task.
#[derive(Debug)]
pub enum LogEvent {
    Attached(Arc<str>),
    Batch { source: Arc<str>, lines: Vec<String> },
    /// The container exited or the stream was closed by the API server.
    Ended(Arc<str>),
    Failed { source: Arc<str>, error: String },
}

/// Spawn a follower task. Drop/abort the returned handle to detach.
pub fn spawn(
    client: kube::Client,
    spec: StreamSpec,
    tx: Sender<LogEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let source = spec.source();
        let api: Api<Pod> = Api::namespaced(client, &spec.namespace);
        let mut backoff = Duration::from_millis(500);

        loop {
            let params = LogParams {
                container: spec.container.clone(),
                follow: true,
                tail_lines: spec.tail,
                since_seconds: spec.since_seconds,
                timestamps: spec.timestamps,
                previous: spec.previous,
                ..Default::default()
            };

            match api.log_stream(&spec.pod, &params).await {
                Ok(stream) => {
                    backoff = Duration::from_millis(500);
                    if tx.send(LogEvent::Attached(source.clone())).await.is_err() {
                        return;
                    }
                    match pump(stream, &source, &tx).await {
                        Ok(()) => {
                            if tx.send(LogEvent::Ended(source.clone())).await.is_err() {
                                return;
                            }
                        }
                        Err(err) => {
                            if tx
                                .send(LogEvent::Failed {
                                    source: source.clone(),
                                    error: err,
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                Err(err) => {
                    if tx
                        .send(LogEvent::Failed {
                            source: source.clone(),
                            error: err.to_string(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }

            // Containers restart, connections drop: retry with capped backoff
            // instead of silently going dead.
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(15));
        }
    })
}

async fn pump<S>(stream: S, source: &Arc<str>, tx: &Sender<LogEvent>) -> Result<(), String>
where
    S: futures::AsyncBufRead + Unpin,
{
    let mut lines = stream.lines();
    let mut batch: Vec<String> = Vec::with_capacity(BATCH_MAX);

    loop {
        match tokio::time::timeout(BATCH_INTERVAL, lines.try_next()).await {
            Ok(Ok(Some(line))) => {
                batch.push(line);
                if batch.len() >= BATCH_MAX {
                    flush(&mut batch, source, tx).await?;
                }
            }
            Ok(Ok(None)) => {
                flush(&mut batch, source, tx).await?;
                return Ok(());
            }
            Ok(Err(err)) => {
                flush(&mut batch, source, tx).await?;
                return Err(err.to_string());
            }
            Err(_timeout) => {
                if !batch.is_empty() {
                    flush(&mut batch, source, tx).await?;
                }
            }
        }
    }
}

async fn flush(
    batch: &mut Vec<String>,
    source: &Arc<str>,
    tx: &Sender<LogEvent>,
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    let lines = std::mem::replace(batch, Vec::with_capacity(BATCH_MAX));
    tx.send(LogEvent::Batch {
        source: source.clone(),
        lines,
    })
    .await
    .map_err(|_| "ui channel closed".to_string())
}

/// Fetch a bounded snapshot of logs without following. Used by `--dump`.
pub async fn snapshot(client: kube::Client, spec: &StreamSpec) -> anyhow::Result<String> {
    let api: Api<Pod> = Api::namespaced(client, &spec.namespace);
    let params = LogParams {
        container: spec.container.clone(),
        follow: false,
        tail_lines: spec.tail,
        since_seconds: spec.since_seconds,
        timestamps: spec.timestamps,
        previous: spec.previous,
        ..Default::default()
    };
    Ok(api.logs(&spec.pod, &params).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_labels_include_container() {
        let spec = StreamSpec {
            namespace: "prod".into(),
            pod: "api-0".into(),
            container: Some("app".into()),
            tail: None,
            since_seconds: None,
            timestamps: false,
            previous: false,
        };
        assert_eq!(&*spec.source(), "prod/api-0:app");
    }
}
