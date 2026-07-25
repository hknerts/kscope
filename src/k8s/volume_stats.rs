//! Actual PersistentVolumeClaim usage, from kubelet's stats/summary endpoint.
//!
//! The Kubernetes API knows how big a PVC was *requested*, never how full it
//! is — that figure only exists on the node that has the volume mounted.
//! kubelet exposes it at `/stats/summary`, the same authenticated node proxy
//! the journal endpoint uses, so this needs `nodes/proxy` and nothing more.
//!
//! Every node is asked in parallel and the answers are merged, because a claim
//! is only reported by the node currently mounting it.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use futures::future::join_all;
use http::Request;
use serde::Deserialize;

/// Live usage of one claim, as the mounting kubelet sees it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VolumeUsage {
    pub used_bytes: f64,
    /// Filesystem capacity, which is what the volume actually offers — often a
    /// little under the requested size once filesystem overhead is taken out.
    pub capacity_bytes: f64,
    pub inodes_used: f64,
    pub inodes_free: f64,
}

impl VolumeUsage {
    pub fn used_pct(&self) -> f64 {
        if self.capacity_bytes <= 0.0 {
            return 0.0;
        }
        (self.used_bytes / self.capacity_bytes) * 100.0
    }
}

// The subset of kubelet's Summary API that matters here. Unknown fields are
// ignored, so kubelet version differences do not break decoding.
#[derive(Debug, Deserialize)]
struct Summary {
    #[serde(default)]
    pods: Vec<PodStats>,
}

#[derive(Debug, Deserialize)]
struct PodStats {
    #[serde(default)]
    volume: Vec<VolumeStats>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VolumeStats {
    /// Absent for non-PVC volumes (emptyDir, configmap, …), which are skipped.
    #[serde(default)]
    pvc_ref: Option<PvcRef>,
    #[serde(default)]
    used_bytes: Option<f64>,
    #[serde(default)]
    capacity_bytes: Option<f64>,
    #[serde(default)]
    inodes_used: Option<f64>,
    #[serde(default)]
    inodes_free: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PvcRef {
    name: String,
    namespace: String,
}

/// Parse one node's summary into `namespace/claim` → usage.
fn parse(body: &str) -> Result<HashMap<String, VolumeUsage>> {
    let summary: Summary =
        serde_json::from_str(body).map_err(|e| anyhow!("decoding kubelet summary: {e}"))?;
    let mut out = HashMap::new();
    for pod in summary.pods {
        for volume in pod.volume {
            let Some(claim) = volume.pvc_ref else {
                continue;
            };
            out.insert(
                format!("{}/{}", claim.namespace, claim.name),
                VolumeUsage {
                    used_bytes: volume.used_bytes.unwrap_or(0.0),
                    capacity_bytes: volume.capacity_bytes.unwrap_or(0.0),
                    inodes_used: volume.inodes_used.unwrap_or(0.0),
                    inodes_free: volume.inodes_free.unwrap_or(0.0),
                },
            );
        }
    }
    Ok(out)
}

/// Ask one node's kubelet for its volume stats.
async fn for_node(client: &kube::Client, node: &str) -> Result<HashMap<String, VolumeUsage>> {
    let request = Request::get(format!("/api/v1/nodes/{node}/proxy/stats/summary"))
        .body(Vec::new())
        .map_err(|e| anyhow!("building stats request: {e}"))?;
    let body = client
        .request_text(request)
        .await
        .map_err(|e| anyhow!("{node}: {e}"))?;
    parse(&body)
}

/// Collect usage across every node, merged into one map.
///
/// Best effort by design: nodes that refuse or time out are skipped rather than
/// failing the whole poll, because partial data is still worth showing. An
/// error is only returned when *every* node failed, so the UI can explain why
/// the column is empty.
pub async fn collect(
    client: &kube::Client,
    nodes: &[String],
) -> Result<HashMap<String, VolumeUsage>> {
    if nodes.is_empty() {
        return Ok(HashMap::new());
    }
    let results = join_all(nodes.iter().map(|n| for_node(client, n))).await;

    let mut merged = HashMap::new();
    let mut last_error = None;
    let mut ok = 0usize;
    for result in results {
        match result {
            Ok(map) => {
                ok += 1;
                merged.extend(map);
            }
            Err(err) => last_error = Some(err),
        }
    }
    if ok == 0 {
        let detail = last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "no nodes reachable".into());
        return Err(anyhow!("volume usage needs get on nodes/proxy — {detail}"));
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "node": {"nodeName": "n1"},
      "pods": [
        {"podRef": {"name": "db-0"},
         "volume": [
           {"name": "data", "usedBytes": 1073741824, "capacityBytes": 10737418240,
            "inodesUsed": 120, "inodesFree": 655240,
            "pvcRef": {"name": "data-db-0", "namespace": "shop"}},
           {"name": "config", "usedBytes": 4096, "capacityBytes": 8192}
         ]},
        {"podRef": {"name": "api-1"}, "volume": []}
      ]
    }"#;

    #[test]
    fn extracts_only_claims_and_keys_them_by_namespace() {
        let got = parse(SAMPLE).unwrap();
        // The configmap volume has no pvcRef and must not appear.
        assert_eq!(got.len(), 1);
        let usage = got.get("shop/data-db-0").unwrap();
        assert_eq!(usage.used_bytes, 1_073_741_824.0);
        assert_eq!(usage.capacity_bytes, 10_737_418_240.0);
        assert_eq!(usage.inodes_used, 120.0);
    }

    #[test]
    fn computes_a_usage_percentage() {
        let usage = parse(SAMPLE).unwrap();
        assert!((usage["shop/data-db-0"].used_pct() - 10.0).abs() < 0.001);
    }

    #[test]
    fn a_zero_capacity_never_divides_by_zero() {
        assert_eq!(VolumeUsage::default().used_pct(), 0.0);
    }

    #[test]
    fn tolerates_missing_fields_and_unknown_ones() {
        let body = r#"{"pods":[{"volume":[
            {"pvcRef":{"name":"c","namespace":"n"},"somethingNew":true}
        ]}]}"#;
        let got = parse(body).unwrap();
        assert_eq!(got["n/c"], VolumeUsage::default());
    }

    #[test]
    fn an_empty_summary_is_not_an_error() {
        assert!(parse(r#"{"node":{}}"#).unwrap().is_empty());
    }

    #[test]
    fn rejects_a_non_summary_body() {
        assert!(parse("<html>403 Forbidden</html>").is_err());
    }
}
