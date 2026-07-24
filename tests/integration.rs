//! End-to-end tests for the parts that do not need a cluster: the log
//! pipeline, the metric store, and the rendering of a styled log line.

use std::sync::Arc;

use kscope::config::{Config, HighlightRule};
use kscope::logs::{classify, Filter, Highlighter, Level, LogBuffer, LogLine};
use kscope::metrics::{fmt_bytes, MetricsStore};
use ratatui::style::Color;
use regex::Regex;

fn line(text: &str) -> LogLine {
    LogLine::new(text.to_string(), Arc::from("ns/pod:container"))
}

/// The default: an unbounded buffer keeps every line of the session, so the
/// first line a container ever emitted is still reachable after a large ingest.
#[test]
fn default_buffer_retains_everything_from_the_first_line() {
    let mut buffer = LogBuffer::new(0); // 0 == unlimited, the default

    for i in 0..250_000u32 {
        buffer.push(line(&format!("INFO handled request id={i}")));
    }

    assert_eq!(buffer.len(), 250_000);
    assert_eq!(buffer.received, 250_000);
    assert_eq!(buffer.dropped, 0, "an unbounded buffer must not evict");
    assert!(buffer.capacity().is_none());

    // The very first line is still there, and still searchable.
    assert!(buffer.view_line(0).unwrap().raw.contains("id=0"));
    let first = Regex::new("id=0$").unwrap();
    assert_eq!(buffer.search_forward(0, &first), Some(0));
}

/// An explicit cap still behaves as a ring buffer: 200 000 lines through a
/// 50 000 line buffer, then a filter change, then a search.
#[test]
fn ingests_a_large_stream_and_stays_bounded() {
    let mut buffer = LogBuffer::new(50_000);

    for i in 0..200_000u32 {
        let text = match i % 100 {
            0 => format!("ERROR upstream refused connection id={i}"),
            7 => format!("WARN retrying request id={i}"),
            _ => format!("INFO handled request id={i} in 12ms"),
        };
        buffer.push(line(&text));
    }

    assert_eq!(buffer.len(), 50_000, "buffer must not grow past capacity");
    assert_eq!(buffer.received, 200_000);
    assert_eq!(buffer.dropped, 150_000);
    assert_eq!(buffer.view_len(), 50_000, "no filter means everything shows");

    // Errors only.
    let mut filter = Filter::default();
    filter.errors_only = true;
    buffer.set_filter(filter);
    assert_eq!(buffer.view_len(), 500);
    assert!(buffer
        .view_line(0)
        .map(|l| l.level == Level::Error)
        .unwrap_or(false));

    // The incremental path must agree with a full rebuild. Pushing into a full
    // buffer also evicts from the front, so the view length is only stable
    // modulo eviction — the invariant that matters is that both paths produce
    // the same view.
    buffer.push(line("ERROR one more failure"));
    // Note: the classifier is token based, so a line merely containing the word
    // "error" would count as one — keep the non-error line free of the word.
    buffer.push(line("INFO everything is fine"));
    let incremental = buffer.view_len();

    let mut same_filter = Filter::default();
    same_filter.errors_only = true;
    buffer.set_filter(same_filter);
    assert_eq!(
        buffer.view_len(),
        incremental,
        "incremental view must match a rebuilt view"
    );

    // The newest error is the last visible line, and search finds it there.
    let last = buffer.view_len() - 1;
    assert!(buffer.view_line(last).unwrap().raw.contains("one more failure"));

    let needle = Regex::new("one more failure").unwrap();
    assert_eq!(buffer.search_forward(0, &needle), Some(last));
    assert_eq!(buffer.search_backward(last, &needle), Some(last));
    assert_eq!(buffer.search_forward(last + 1, &needle), None);
}

#[test]
fn include_and_exclude_filters_compose() {
    let mut buffer = LogBuffer::new(1_000);
    buffer.push(line("INFO GET /healthz 200"));
    buffer.push(line("INFO GET /orders 200"));
    buffer.push(line("INFO POST /orders 500"));

    let mut filter = Filter::default();
    filter.include = Some(Regex::new("/orders").unwrap());
    filter.exclude = Some(Regex::new("POST").unwrap());
    buffer.set_filter(filter);

    assert_eq!(buffer.view_len(), 1);
    assert!(buffer.view_line(0).unwrap().raw.contains("GET /orders"));
    assert!(buffer.filter.is_active());
}

#[test]
fn exports_only_the_visible_lines() {
    let mut buffer = LogBuffer::new(100);
    buffer.push(line("INFO first"));
    buffer.push(line("ERROR second"));

    let mut filter = Filter::default();
    filter.errors_only = true;
    buffer.set_filter(filter);

    let text = buffer.to_plain_text();
    assert!(text.contains("ERROR second"));
    assert!(!text.contains("INFO first"));
    assert!(text.ends_with('\n'));
}

#[test]
fn classification_survives_common_log_formats() {
    let cases = [
        (r#"{"level":"error","msg":"db down"}"#, Level::Error),
        ("time=2024-05-01 level=warn msg=slow", Level::Warn),
        ("2024-05-01T00:00:00Z DEBUG cache warm", Level::Debug),
        ("I0501 10:11:12.000 klog style message", Level::Info),
        ("panic: runtime error: index out of range", Level::Fatal),
    ];
    for (text, expected) in cases {
        assert_eq!(classify(text), expected, "misclassified: {text}");
    }
}

#[test]
fn highlighter_marks_search_hits_and_user_rules() {
    let mut config = Config::default();
    config.highlight.push(HighlightRule {
        pattern: "order_id=[0-9]+".into(),
        fg: "#ff8800".into(),
        bold: true,
    });
    let highlighter = Highlighter::new(&config.theme, &config.highlight);

    let entry = line("ERROR payment failed order_id=42 status=500");
    let search = Regex::new("payment").unwrap();
    let rendered = highlighter.render(&entry, Some(&search), false);

    // The whole line is reconstructed exactly, split into styled spans.
    let joined: String = rendered.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(joined, &*entry.raw);
    assert!(rendered.spans.len() > 1, "expected multiple styled spans");

    // The search hit uses the theme's match background.
    let hit = rendered
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "payment")
        .expect("search hit present");
    assert_eq!(hit.style.bg, Some(Color::Yellow));

    // The user rule colours the order id.
    let custom = rendered
        .spans
        .iter()
        .find(|s| s.content.contains("order_id=42"))
        .expect("user rule applied");
    assert_eq!(custom.style.fg, Some(Color::Rgb(0xff, 0x88, 0x00)));
}

#[test]
fn timestamps_can_be_stripped_at_render_time() {
    let entry = line("2024-05-01T10:11:12.000Z INFO ready");
    let config = Config::default();
    let highlighter = Highlighter::new(&config.theme, &[]);

    let with_ts: String = highlighter
        .render(&entry, None, false)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    let without_ts: String = highlighter
        .render(&entry, None, true)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();

    assert!(with_ts.starts_with("2024-05-01T"));
    assert_eq!(without_ts, "INFO ready");
}

#[test]
fn metric_store_tracks_node_pod_and_container_separately() {
    let mut store = MetricsStore::new(32);

    for step in 0..5 {
        store.record_node("node-a", 1000.0 + step as f64, 8.0 * 1024.0 * 1024.0 * 1024.0);
        store.record_pod(
            "prod",
            "api-0",
            &[
                ("app".into(), 200.0 + step as f64, 512.0 * 1024.0 * 1024.0),
                ("envoy".into(), 30.0, 64.0 * 1024.0 * 1024.0),
            ],
        );
    }

    let node = store.nodes.get("node-a").expect("node recorded");
    node.usage.cpu.last();
    assert_eq!(node.usage.cpu.last(), 1004.0);

    let pod = store.pods.get("prod/api-0").expect("pod recorded");
    assert_eq!(pod.containers.len(), 2, "containers tracked individually");
    assert_eq!(pod.usage.cpu.last(), 234.0, "pod total is the container sum");
    assert_eq!(fmt_bytes(pod.usage.mem.last()), "576.0Mi");

    // The sparkline is scaled into the 0..100 range ratatui expects.
    let spark = pod.usage.cpu.sparkline(16, pod.usage.cpu.max());
    assert_eq!(spark.len(), 5);
    assert!(spark.iter().all(|v| *v <= 100));

    // A pod that disappears from the cluster is dropped.
    store.retain_pods(&std::collections::HashSet::new());
    assert!(store.pods.is_empty());
    assert!(store.nodes.contains_key("node-a"), "nodes are kept");
}

#[test]
fn config_round_trips_through_toml() {
    let raw = r#"
        [general]
        max_fps = 60

        [logs]
        buffer_lines = 250000
        smart_case = false

        [metrics]
        refresh_ms = 2000
        critical_pct = 95.0

        [[highlight]]
        pattern = "trace_id=[a-f0-9]+"
        fg = "cyan"
    "#;
    let config: Config = toml::from_str(raw).expect("valid config");
    assert_eq!(config.general.max_fps, 60);
    assert_eq!(config.logs.buffer_lines, 250_000);
    assert!(!config.logs.smart_case);
    assert_eq!(config.metrics.refresh_ms, 2_000);
    assert_eq!(config.metrics.critical_pct, 95.0);
    assert_eq!(config.highlight.len(), 1);
    // Unset values keep their defaults.
    assert_eq!(config.metrics.warn_pct, 75.0);
}
