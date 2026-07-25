//! Node service logs, via the kubelet log query endpoint.
//!
//! Kubernetes 1.27 added `/logs` to the kubelet's authenticated proxy, which
//! serves the node's own journal — kubelet, containerd, the kernel. It is the
//! one place a "why did this node misbehave" question can be answered without
//! SSH, and nothing else in a TUI surfaces it.
//!
//! It is gated: the `NodeLogQuery` feature gate plus kubelet's
//! `enableSystemLogHandler` and `enableSystemLogQuery` all have to be on. When
//! they are not, the API server answers with a clear error, which is surfaced
//! as-is rather than dressed up.

use anyhow::{anyhow, Result};
use http::Request;

/// Services worth offering by default. `kubelet` is the one that matters most;
/// the rest are present on most Linux nodes running containerd.
pub const COMMON_SERVICES: [&str; 3] = ["kubelet", "containerd", "kernel"];

/// Fetch `service` logs from `node`.
///
/// `tail` caps the number of lines. `since`/`until` are not exposed yet — the
/// endpoint's filtering is journald-specific and varies by node OS, so kscope
/// keeps to the portable subset.
pub async fn fetch(
    client: &kube::Client,
    node: &str,
    service: &str,
    tail: Option<i64>,
) -> Result<String> {
    // The endpoint is a node subresource proxy, so it is a raw request rather
    // than anything `Api<T>` models.
    let mut query = format!("query={service}");
    if let Some(tail) = tail.filter(|t| *t > 0) {
        query.push_str(&format!("&tailLines={tail}"));
    }
    let url = format!("/api/v1/nodes/{node}/proxy/logs/?{query}");

    let request = Request::get(&url)
        .body(Vec::new())
        .map_err(|e| anyhow!("building node log request: {e}"))?;

    let body = client
        .request_text(request)
        .await
        .map_err(|e| anyhow!(explain(&e.to_string())))?;

    if body.trim().is_empty() {
        return Err(anyhow!(
            "node {node} returned no {service} logs — the service may not exist on this node"
        ));
    }
    // With NodeLogQuery disabled the endpoint ignores `query=` entirely and
    // falls back to serving a browsable /var/log index. That is a 200 with an
    // HTML directory listing in it, which must not be mistaken for logs.
    if is_directory_index(&body) {
        return Err(anyhow!(
            "node {node} served a /var/log directory index instead of {service} logs.\n\n\
             This node does not have the NodeLogQuery feature gate enabled, so the log query \
             API is unavailable and kubelet fell back to its legacy file browser. Needs \
             Kubernetes 1.27+ with NodeLogQuery on, plus kubelet's enableSystemLogHandler and \
             enableSystemLogQuery both true."
        ));
    }
    Ok(body)
}

/// kubelet's legacy `/var/log` browser answers with a minimal HTML page. Real
/// journal output never does, so this is a safe thing to reject.
fn is_directory_index(body: &str) -> bool {
    let head = body.trim_start();
    let lower = head.to_ascii_lowercase();
    lower.starts_with("<!doctype html") || lower.starts_with("<html") || lower.starts_with("<pre>")
}

/// Turn the two failure modes people actually hit into something actionable.
fn explain(error: &str) -> String {
    if error.contains("404") || error.contains("not found") {
        format!(
            "{error}\n\nNode logs need Kubernetes 1.27+ with the NodeLogQuery feature gate, and \
             kubelet's enableSystemLogHandler + enableSystemLogQuery both set to true."
        )
    } else if error.contains("403") || error.contains("Forbidden") {
        format!("{error}\n\nThis needs get on nodes/proxy, which the view role does not grant.")
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_a_missing_endpoint_as_a_feature_gate_problem() {
        let msg = explain("404 page not found");
        assert!(msg.contains("NodeLogQuery"));
        assert!(msg.contains("enableSystemLogQuery"));
    }

    #[test]
    fn explains_a_forbidden_response_as_an_rbac_problem() {
        assert!(explain("403 Forbidden").contains("nodes/proxy"));
    }

    #[test]
    fn passes_other_errors_through_untouched() {
        assert_eq!(explain("connection reset"), "connection reset");
    }

    #[test]
    fn recognises_kubelets_legacy_var_log_index() {
        // Exactly what a node without the NodeLogQuery gate answers with — a
        // 200 that would otherwise be shown as if it were journal output.
        let body = "<!doctype html>\n<meta name=\"viewport\" content=\"width=device-width\">\n\
                    <pre>\n<a href=\"alternatives.log\">alternatives.log</a>\n\
                    <a href=\"containers/\">containers/</a>\n</pre>\n";
        assert!(is_directory_index(body));
        assert!(is_directory_index("<html><body>nope</body></html>"));
        assert!(is_directory_index("  <PRE>\n<a href=\"x\">x</a>\n</PRE>"));
    }

    #[test]
    fn real_journal_output_is_not_mistaken_for_an_index() {
        assert!(!is_directory_index(
            "Jul 25 09:10:46 node kubelet[1234]: I0725 09:10:46.123 server.go:1 Started kubelet"
        ));
        assert!(!is_directory_index("{\"msg\":\"<html> in a log line\"}"));
        assert!(!is_directory_index(""));
    }
}
