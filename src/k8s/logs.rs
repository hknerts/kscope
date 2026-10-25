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

use futures::AsyncBufReadExt;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::jiff::Timestamp;
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
    /// The fleet member the pod lives on, when logs are merged across a
    /// fleet. Part of the source label, so two clusters' `api-0` stay apart.
    pub context: Option<String>,
}

impl StreamSpec {
    /// Stable identifier used as the per-line source label:
    /// `ns/pod:container`, prefixed `context|` within a fleet.
    pub fn source(&self) -> Arc<str> {
        let local = match &self.container {
            Some(c) => format!("{}/{}:{}", self.namespace, self.pod, c),
            None => format!("{}/{}", self.namespace, self.pod),
        };
        match &self.context {
            Some(context) => Arc::from(format!("{context}|{local}")),
            None => Arc::from(local),
        }
    }
}

/// Events emitted by a streaming task.
#[derive(Debug)]
pub enum LogEvent {
    Attached(Arc<str>),
    Batch {
        source: Arc<str>,
        lines: Vec<String>,
    },
    /// The container exited or the stream was closed by the API server.
    Ended(Arc<str>),
    Failed {
        source: Arc<str>,
        error: String,
    },
}

/// Spawn a follower task. Drop/abort the returned handle to detach.
///
/// Reconnects never replay history. The API server is always asked for
/// timestamps, so the task knows exactly which line it saw last: a reconnect
/// resumes from there with `sinceTime` and skips anything at or before it. The
/// prefix is stripped again before the line leaves the task unless the user
/// asked to see timestamps.
pub fn spawn(
    client: kube::Client,
    spec: StreamSpec,
    tx: Sender<LogEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let source = spec.source();
        let api: Api<Pod> = Api::namespaced(client, &spec.namespace);
        let mut backoff = Duration::from_millis(500);
        let mut cursor = Cursor::default();

        loop {
            // The first attach honours tail/since as configured; a resume
            // only wants what came after the last line already delivered.
            let params = LogParams {
                container: spec.container.clone(),
                follow: true,
                tail_lines: if cursor.last.is_some() {
                    None
                } else {
                    spec.tail
                },
                since_seconds: if cursor.last.is_some() {
                    None
                } else {
                    spec.since_seconds
                },
                since_time: cursor.last,
                timestamps: true,
                previous: spec.previous,
                ..Default::default()
            };

            match api.log_stream(&spec.pod, &params).await {
                Ok(stream) => {
                    if tx.send(LogEvent::Attached(source.clone())).await.is_err() {
                        return;
                    }
                    cursor.resume();
                    let before = cursor.delivered;
                    let outcome = pump(stream, &source, &tx, &mut cursor, spec.timestamps).await;
                    // Only a stream that actually delivered something earns a
                    // fast retry. A terminated container ends its stream
                    // immediately, and must not be re-polled twice a second.
                    if cursor.delivered > before {
                        backoff = Duration::from_millis(500);
                    }
                    let event = match outcome {
                        Ok(()) => LogEvent::Ended(source.clone()),
                        Err(error) => LogEvent::Failed {
                            source: source.clone(),
                            error,
                        },
                    };
                    if tx.send(event).await.is_err() {
                        return;
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

/// Where a stream got to, so a reconnect can pick up without duplicates.
#[derive(Debug, Default)]
struct Cursor {
    /// Timestamp of the newest line delivered.
    last: Option<Timestamp>,
    /// Lines delivered over the task's lifetime.
    delivered: u64,
    /// How many of the delivered lines carry exactly `last`. Two lines can
    /// share a stamp, so "at or before `last`" alone would drop the second.
    at_last: u64,
    /// Lines stamped `last` still to skip in a replay after a reconnect.
    replay: u64,
}

impl Cursor {
    /// A new stream is about to replay from `last` (`sinceTime` is inclusive):
    /// the lines already delivered with that stamp come again and are skipped.
    fn resume(&mut self) {
        self.replay = self.at_last;
    }

    /// Decide what to do with one raw, timestamp-prefixed line: `None` when it
    /// was already delivered before a reconnect, otherwise the text to show.
    fn accept(&mut self, raw: String, keep_timestamp: bool) -> Option<String> {
        let (stamp, body_at) = match raw.split_once(' ') {
            Some((head, _)) => match head.parse::<Timestamp>() {
                Ok(ts) => (Some(ts), head.len() + 1),
                Err(_) => (None, 0),
            },
            None => match raw.parse::<Timestamp>() {
                // A blank line still carries its timestamp.
                Ok(ts) => (Some(ts), raw.len()),
                Err(_) => (None, 0),
            },
        };
        if let Some(ts) = stamp {
            match self.last {
                Some(last) if ts < last => return None,
                Some(last) if ts == last => {
                    if self.replay > 0 {
                        self.replay -= 1;
                        return None;
                    }
                    self.at_last += 1;
                }
                _ => {
                    self.last = Some(ts);
                    self.at_last = 1;
                    self.replay = 0;
                }
            }
        }
        self.delivered += 1;
        if keep_timestamp || body_at == 0 {
            Some(raw)
        } else {
            Some(raw[body_at.min(raw.len())..].to_string())
        }
    }
}

/// Read lines and forward them in batches.
///
/// A batch is flushed when it is full or [`BATCH_INTERVAL`] after its *first*
/// line arrived — not after the stream goes quiet, which would hold a steady
/// trickle of lines back until 512 had piled up.
async fn pump<S>(
    stream: S,
    source: &Arc<str>,
    tx: &Sender<LogEvent>,
    cursor: &mut Cursor,
    keep_timestamp: bool,
) -> Result<(), String>
where
    S: futures::AsyncBufRead + Unpin,
{
    let mut reader = stream;
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut batch: Vec<String> = Vec::with_capacity(BATCH_MAX);
    let deadline = tokio::time::sleep(BATCH_INTERVAL);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            biased;

            // `read_until` keeps whatever it already consumed in `buf`, so
            // losing this race to the deadline never drops part of a line.
            read = reader.read_until(b'\n', &mut buf) => match read {
                Ok(0) => {
                    if !buf.is_empty() {
                        take_line(&mut buf, &mut batch, cursor, keep_timestamp);
                    }
                    flush(&mut batch, source, tx).await?;
                    return Ok(());
                }
                Ok(_) => {
                    if buf.last() != Some(&b'\n') {
                        // Partial line at end of stream; the next read
                        // returns 0 and finishes it.
                        continue;
                    }
                    let was_empty = batch.is_empty();
                    take_line(&mut buf, &mut batch, cursor, keep_timestamp);
                    if was_empty && !batch.is_empty() {
                        deadline
                            .as_mut()
                            .reset(tokio::time::Instant::now() + BATCH_INTERVAL);
                    }
                    if batch.len() >= BATCH_MAX {
                        flush(&mut batch, source, tx).await?;
                    }
                }
                Err(err) => {
                    flush(&mut batch, source, tx).await?;
                    return Err(err.to_string());
                }
            },
            _ = &mut deadline, if !batch.is_empty() => {
                flush(&mut batch, source, tx).await?;
            }
        }
    }
}

/// Turn the bytes in `buf` into a line on `batch`. Invalid UTF-8 is replaced
/// rather than treated as an error: one binary byte in a container's output
/// must not tear the stream down.
fn take_line(buf: &mut Vec<u8>, batch: &mut Vec<String>, cursor: &mut Cursor, keep: bool) {
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    let raw = match String::from_utf8(std::mem::take(buf)) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    if let Some(line) = cursor.accept(raw, keep) {
        batch.push(line);
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
            context: None,
        };
        assert_eq!(&*spec.source(), "prod/api-0:app");
        let fleet = StreamSpec {
            context: Some("stg-eu".into()),
            ..spec
        };
        assert_eq!(&*fleet.source(), "stg-eu|prod/api-0:app");
    }

    #[test]
    fn a_resume_skips_lines_already_delivered() {
        let mut cursor = Cursor::default();
        let a = "2024-05-01T10:00:00.100000000Z first".to_string();
        let b = "2024-05-01T10:00:00.200000000Z second".to_string();
        assert_eq!(cursor.accept(a.clone(), false).as_deref(), Some("first"));
        assert_eq!(cursor.accept(b.clone(), false).as_deref(), Some("second"));
        // A reconnect with sinceTime replays from the start of that second.
        cursor.resume();
        assert_eq!(cursor.accept(a, false), None);
        assert_eq!(cursor.accept(b, false), None);
        let c = "2024-05-01T10:00:01Z third".to_string();
        assert_eq!(cursor.accept(c.clone(), true), Some(c));
        assert_eq!(cursor.delivered, 3);
    }

    #[test]
    fn lines_that_share_a_timestamp_are_all_delivered() {
        // Two lines can carry the same stamp (one write, a coarse clock).
        // Within one stream both are new; only a replay may skip them.
        let mut cursor = Cursor::default();
        let a = "2024-05-01T10:00:00.100000000Z first".to_string();
        let b = "2024-05-01T10:00:00.100000000Z second".to_string();
        assert_eq!(cursor.accept(a.clone(), false).as_deref(), Some("first"));
        assert_eq!(cursor.accept(b.clone(), false).as_deref(), Some("second"));
        // A reconnect replays both, and then a third line with that stamp
        // that was never seen.
        cursor.resume();
        assert_eq!(cursor.accept(a, false), None);
        assert_eq!(cursor.accept(b, false), None);
        let c = "2024-05-01T10:00:00.100000000Z third".to_string();
        assert_eq!(cursor.accept(c, false).as_deref(), Some("third"));
        assert_eq!(cursor.delivered, 3);
    }

    #[test]
    fn lines_without_a_timestamp_pass_through() {
        let mut cursor = Cursor::default();
        assert_eq!(
            cursor.accept("plain".into(), false).as_deref(),
            Some("plain")
        );
        assert_eq!(
            cursor
                .accept("2024-05-01T10:00:00Z".into(), false)
                .as_deref(),
            Some("")
        );
    }

    #[tokio::test]
    async fn a_trickle_is_flushed_on_time_not_when_the_batch_fills() {
        // Lines arriving every 30 ms never let a per-line timeout expire; the
        // batch must still go out about 100 ms after its first line.
        let (mut writer, reader) = tokio::io::duplex(4096);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let source: Arc<str> = Arc::from("ns/pod:c");
        let task = tokio::spawn(async move {
            let reader = tokio_util_compat(reader);
            let mut cursor = Cursor::default();
            pump(reader, &source, &tx, &mut cursor, false).await
        });
        let started = std::time::Instant::now();
        let writer_task = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            for i in 0..40 {
                let line = format!("2024-05-01T10:00:{:02}.000000001Z line {i}\n", i);
                writer.write_all(line.as_bytes()).await.unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        });
        let Some(LogEvent::Batch { lines, .. }) = rx.recv().await else {
            panic!("expected a batch");
        };
        assert!(started.elapsed() < Duration::from_millis(400));
        assert!(!lines.is_empty() && lines.len() < 40);
        assert_eq!(lines[0], "line 0");
        writer_task.abort();
        task.abort();
    }

    /// Minimal tokio → futures `AsyncBufRead` adapter for the test above.
    fn tokio_util_compat(reader: tokio::io::DuplexStream) -> impl futures::AsyncBufRead + Unpin {
        use futures::StreamExt;
        let stream = futures::stream::unfold(reader, |mut r| async move {
            use tokio::io::AsyncReadExt;
            let mut chunk = vec![0u8; 512];
            match r.read(&mut chunk).await {
                Ok(0) | Err(_) => None,
                Ok(n) => {
                    chunk.truncate(n);
                    Some((Ok::<_, std::io::Error>(chunk), r))
                }
            }
        })
        .boxed();
        futures::TryStreamExt::into_async_read(stream)
    }
}
