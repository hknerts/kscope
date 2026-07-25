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
    Ok(body)
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
}
