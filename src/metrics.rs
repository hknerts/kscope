//! Metric storage: parsed Kubernetes quantities plus a bounded time series per
//! tracked resource, so the UI can draw live sparklines without recomputation.

use std::collections::{BTreeMap, VecDeque};

/// Parse a Kubernetes CPU quantity into millicores.
///
/// Handles `"250m"`, `"1"`, `"1.5"`, `"123456789n"`, `"500u"`, `"2k"`.
pub fn parse_cpu(q: &str) -> f64 {
    let q = q.trim();
    if q.is_empty() {
        return 0.0;
    }
    let (num, suffix) = split_quantity(q);
    let value: f64 = num.parse().unwrap_or(0.0);
    match suffix {
        "n" => value / 1_000_000.0,
        "u" => value / 1_000.0,
        "m" => value,
        "k" => value * 1_000_000.0,
        "" => value * 1_000.0,
        _ => value * 1_000.0,
    }
}

/// Parse a Kubernetes memory quantity into bytes.
pub fn parse_memory(q: &str) -> f64 {
    let q = q.trim();
    if q.is_empty() {
        return 0.0;
    }
    let (num, suffix) = split_quantity(q);
    let value: f64 = num.parse().unwrap_or(0.0);
    let factor = match suffix {
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        "k" | "K" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "m" => 1e-3,
        _ => 1.0,
    };
    value * factor
}

fn split_quantity(q: &str) -> (&str, &str) {
    let idx = q
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(q.len());
    (&q[..idx], &q[idx..])
}

/// Human readable byte formatting (binary units).
pub fn fmt_bytes(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "Ki", "Mi", "Gi", "Ti", "Pi"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// Human readable CPU formatting (millicores or cores).
pub fn fmt_cpu(milli: f64) -> String {
    if milli >= 1000.0 {
        format!("{:.2}", milli / 1000.0)
    } else {
        format!("{milli:.0}m")
    }
}

/// A bounded series of samples. Push is O(1); `max` is recomputed lazily.
#[derive(Debug, Clone)]
pub struct Series {
    data: VecDeque<f64>,
    cap: usize,
}

impl Series {
    pub fn new(cap: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(cap.min(1024)),
            cap: cap.max(8),
        }
    }

    pub fn push(&mut self, value: f64) {
        if self.data.len() == self.cap {
            self.data.pop_front();
        }
        self.data.push_back(value);
    }

    pub fn last(&self) -> f64 {
        self.data.back().copied().unwrap_or(0.0)
    }

    pub fn max(&self) -> f64 {
        self.data.iter().copied().fold(0.0, f64::max)
    }

    pub fn avg(&self) -> f64 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().sum::<f64>() / self.data.len() as f64
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Last `n` samples scaled into `u64` for [`ratatui::widgets::Sparkline`].
    pub fn sparkline(&self, n: usize, scale: f64) -> Vec<u64> {
        let skip = self.data.len().saturating_sub(n);
        self.data
            .iter()
            .skip(skip)
            .map(|v| {
                if scale <= 0.0 {
                    0
                } else {
                    ((v / scale) * 100.0).round().clamp(0.0, 100.0) as u64
                }
            })
            .collect()
    }
}

/// CPU + memory usage pair with history.
#[derive(Debug, Clone)]
pub struct Usage {
    pub cpu: Series,
    pub mem: Series,
}

impl Usage {
    pub fn new(history: usize) -> Self {
        Self {
            cpu: Series::new(history),
            mem: Series::new(history),
        }
    }

    pub fn record(&mut self, cpu_milli: f64, mem_bytes: f64) {
        self.cpu.push(cpu_milli);
        self.mem.push(mem_bytes);
    }
}

#[derive(Debug, Clone)]
pub struct ContainerMetrics {
    pub name: String,
    pub usage: Usage,
    /// From the pod spec, if declared.
    pub cpu_request: f64,
    pub cpu_limit: f64,
    pub mem_request: f64,
    pub mem_limit: f64,
    pub restarts: i32,
    pub ready: bool,
    pub state: String,
}

impl ContainerMetrics {
    fn new(name: String, history: usize) -> Self {
        Self {
            name,
            usage: Usage::new(history),
            cpu_request: 0.0,
            cpu_limit: 0.0,
            mem_request: 0.0,
            mem_limit: 0.0,
            restarts: 0,
            ready: false,
            state: "-".into(),
        }
    }

    /// Memory usage against the limit, in percent (0 when no limit is set).
    pub fn mem_pct(&self) -> f64 {
        pct(self.usage.mem.last(), self.mem_limit)
    }

    pub fn cpu_pct(&self) -> f64 {
        pct(self.usage.cpu.last(), self.cpu_limit)
    }
}

#[derive(Debug, Clone)]
pub struct PodMetrics {
    pub namespace: String,
    pub name: String,
    pub node: String,
    pub phase: String,
    pub usage: Usage,
    pub containers: Vec<ContainerMetrics>,
    pub cpu_request: f64,
    pub cpu_limit: f64,
    pub mem_request: f64,
    pub mem_limit: f64,
    /// Set when the last poll did not include this pod.
    pub stale: bool,
}

impl PodMetrics {
    pub fn key(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    pub fn restarts(&self) -> i32 {
        self.containers.iter().map(|c| c.restarts).sum()
    }

    pub fn ready_string(&self) -> String {
        let ready = self.containers.iter().filter(|c| c.ready).count();
        format!("{}/{}", ready, self.containers.len())
    }
}

#[derive(Debug, Clone)]
pub struct NodeMetrics {
    pub name: String,
    pub usage: Usage,
    pub cpu_capacity: f64,
    pub mem_capacity: f64,
    pub cpu_allocatable: f64,
    pub mem_allocatable: f64,
    pub pods: usize,
    pub ready: bool,
    pub version: String,
}

impl NodeMetrics {
    pub fn cpu_pct(&self) -> f64 {
        pct(self.usage.cpu.last(), self.cpu_allocatable)
    }
    pub fn mem_pct(&self) -> f64 {
        pct(self.usage.mem.last(), self.mem_allocatable)
    }
}

pub fn pct(value: f64, total: f64) -> f64 {
    if total <= 0.0 {
        0.0
    } else {
        (value / total) * 100.0
    }
}

/// Everything the metrics view knows about the cluster.
#[derive(Debug, Default)]
pub struct MetricsStore {
    pub nodes: BTreeMap<String, NodeMetrics>,
    /// Keyed by `namespace/pod`.
    pub pods: BTreeMap<String, PodMetrics>,
    pub history: usize,
    pub last_update: Option<std::time::Instant>,
    /// Set when metrics-server is unreachable, so the UI can explain itself.
    pub error: Option<String>,
}

impl MetricsStore {
    pub fn new(history: usize) -> Self {
        Self {
            history: history.max(8),
            ..Default::default()
        }
    }

    /// Cluster-wide sums over the last sample.
    pub fn cluster_totals(&self) -> (f64, f64, f64, f64) {
        let mut cpu_used = 0.0;
        let mut cpu_cap = 0.0;
        let mut mem_used = 0.0;
        let mut mem_cap = 0.0;
        for n in self.nodes.values() {
            cpu_used += n.usage.cpu.last();
            cpu_cap += n.cpu_allocatable;
            mem_used += n.usage.mem.last();
            mem_cap += n.mem_allocatable;
        }
        (cpu_used, cpu_cap, mem_used, mem_cap)
    }

    /// Record one node sample, creating the entry on first sight.
    pub fn record_node(&mut self, name: &str, cpu_milli: f64, mem_bytes: f64) {
        let history = self.history;
        let entry = self
            .nodes
            .entry(name.to_string())
            .or_insert_with(|| NodeMetrics {
                name: name.to_string(),
                usage: Usage::new(history),
                cpu_capacity: 0.0,
                mem_capacity: 0.0,
                cpu_allocatable: 0.0,
                mem_allocatable: 0.0,
                pods: 0,
                ready: true,
                version: String::new(),
            });
        entry.usage.record(cpu_milli, mem_bytes);
    }

    /// Record one pod sample with its per-container breakdown.
    pub fn record_pod(&mut self, namespace: &str, name: &str, containers: &[(String, f64, f64)]) {
        let history = self.history;
        let key = format!("{namespace}/{name}");
        let entry = self.pods.entry(key).or_insert_with(|| PodMetrics {
            namespace: namespace.to_string(),
            name: name.to_string(),
            node: String::new(),
            phase: String::new(),
            usage: Usage::new(history),
            containers: Vec::new(),
            cpu_request: 0.0,
            cpu_limit: 0.0,
            mem_request: 0.0,
            mem_limit: 0.0,
            stale: false,
        });

        let mut cpu_total = 0.0;
        let mut mem_total = 0.0;
        for (cname, cpu, mem) in containers {
            cpu_total += cpu;
            mem_total += mem;
            if let Some(c) = entry.containers.iter_mut().find(|c| &c.name == cname) {
                c.usage.record(*cpu, *mem);
            } else {
                let mut c = ContainerMetrics::new(cname.clone(), history);
                c.usage.record(*cpu, *mem);
                entry.containers.push(c);
            }
        }
        entry.usage.record(cpu_total, mem_total);
        entry.stale = false;
    }

    /// Drop pods that disappeared from the cluster.
    pub fn retain_pods(&mut self, keys: &std::collections::HashSet<String>) {
        self.pods.retain(|k, _| keys.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_quantities() {
        assert_eq!(parse_cpu("250m"), 250.0);
        assert_eq!(parse_cpu("1"), 1000.0);
        assert_eq!(parse_cpu("1.5"), 1500.0);
        assert!((parse_cpu("123456789n") - 123.456789).abs() < 1e-6);
        assert_eq!(parse_cpu(""), 0.0);
    }

    #[test]
    fn parses_memory_quantities() {
        assert_eq!(parse_memory("1Ki"), 1024.0);
        assert_eq!(parse_memory("2Mi"), 2.0 * 1024.0 * 1024.0);
        assert_eq!(parse_memory("1G"), 1e9);
        assert_eq!(parse_memory("512"), 512.0);
    }

    #[test]
    fn formats_human_readable() {
        assert_eq!(fmt_bytes(1536.0), "1.5Ki");
        assert_eq!(fmt_cpu(250.0), "250m");
        assert_eq!(fmt_cpu(2500.0), "2.50");
    }

    #[test]
    fn series_is_bounded() {
        let mut s = Series::new(10);
        for i in 0..100 {
            s.push(i as f64);
        }
        assert_eq!(s.sparkline(64, 99.0).len(), 10);
        assert_eq!(s.last(), 99.0);
        assert_eq!(s.max(), 99.0);
    }

    #[test]
    fn store_aggregates_containers_into_pod_total() {
        let mut store = MetricsStore::new(16);
        store.record_pod(
            "default",
            "web",
            &[
                ("app".to_string(), 100.0, 1024.0),
                ("sidecar".to_string(), 50.0, 512.0),
            ],
        );
        let pod = store.pods.get("default/web").unwrap();
        assert_eq!(pod.usage.cpu.last(), 150.0);
        assert_eq!(pod.usage.mem.last(), 1536.0);
        assert_eq!(pod.containers.len(), 2);
    }

    #[test]
    fn parses_every_cpu_suffix_metrics_server_emits() {
        assert!((parse_cpu("500u") - 0.5).abs() < 1e-9);
        assert_eq!(parse_cpu("2k"), 2_000_000.0);
        assert_eq!(parse_cpu("  100m "), 100.0);
        assert_eq!(parse_cpu("0"), 0.0);
        // Garbage is zero, never a panic.
        assert_eq!(parse_cpu("lots"), 0.0);
        assert_eq!(parse_cpu("m"), 0.0);
    }

    #[test]
    fn parses_every_memory_suffix() {
        assert_eq!(parse_memory("1Gi"), 1024f64.powi(3));
        assert_eq!(parse_memory("1Ti"), 1024f64.powi(4));
        assert_eq!(parse_memory("1Pi"), 1024f64.powi(5));
        assert_eq!(parse_memory("1Ei"), 1024f64.powi(6));
        assert_eq!(parse_memory("1.5Mi"), 1.5 * 1024.0 * 1024.0);
        assert_eq!(parse_memory("2k"), 2e3);
        assert_eq!(parse_memory("2K"), 2e3);
        assert_eq!(parse_memory("3M"), 3e6);
        assert_eq!(parse_memory("1T"), 1e12);
        assert_eq!(parse_memory("1P"), 1e15);
        // Kubernetes does print millibytes for odd requests.
        assert_eq!(parse_memory("1500m"), 1.5);
        assert_eq!(parse_memory(" 64Mi "), 64.0 * 1024.0 * 1024.0);
        assert_eq!(parse_memory(""), 0.0);
        assert_eq!(parse_memory("Mi"), 0.0);
    }

    #[test]
    fn formats_bytes_at_unit_boundaries() {
        assert_eq!(fmt_bytes(0.0), "0B");
        assert_eq!(fmt_bytes(1023.0), "1023B");
        assert_eq!(fmt_bytes(1024.0), "1.0Ki");
        assert_eq!(fmt_bytes(1024f64.powi(3) * 2.0), "2.0Gi");
        // Past the largest unit the number grows instead of the unit.
        assert_eq!(fmt_bytes(1024f64.powi(6)), "1024.0Pi");
    }

    #[test]
    fn formats_cpu_as_millicores_below_a_core() {
        assert_eq!(fmt_cpu(0.0), "0m");
        assert_eq!(fmt_cpu(999.0), "999m");
        assert_eq!(fmt_cpu(1000.0), "1.00");
        assert_eq!(fmt_cpu(12_340.0), "12.34");
    }

    #[test]
    fn percentages_of_nothing_are_zero() {
        assert_eq!(pct(50.0, 200.0), 25.0);
        assert_eq!(pct(50.0, 0.0), 0.0);
        assert_eq!(pct(50.0, -1.0), 0.0);
        // Over the limit is reported as such; the gauge clamps, not this.
        assert_eq!(pct(300.0, 200.0), 150.0);
    }

    #[test]
    fn an_empty_series_reads_as_zero() {
        let s = Series::new(16);
        assert!(s.is_empty());
        assert_eq!(s.last(), 0.0);
        assert_eq!(s.max(), 0.0);
        assert_eq!(s.avg(), 0.0);
        assert!(s.sparkline(10, 100.0).is_empty());
    }

    #[test]
    fn series_capacity_has_a_floor() {
        let mut s = Series::new(0);
        for i in 0..20 {
            s.push(i as f64);
        }
        assert_eq!(s.sparkline(100, 1.0).len(), 8);
        assert_eq!(s.avg(), (12..20).sum::<i32>() as f64 / 8.0);
    }

    #[test]
    fn sparkline_takes_the_newest_samples_and_clamps_them() {
        let mut s = Series::new(16);
        for v in [10.0, 50.0, 100.0, 250.0, -5.0] {
            s.push(v);
        }
        assert_eq!(s.sparkline(3, 100.0), vec![100, 100, 0]);
        assert_eq!(s.sparkline(10, 200.0), vec![5, 25, 50, 100, 0]);
        // A zero scale (no limit set) draws a flat line, not NaN.
        assert_eq!(s.sparkline(2, 0.0), vec![0, 0]);
    }

    #[test]
    fn repeated_pod_samples_extend_the_same_containers() {
        let mut store = MetricsStore::new(16);
        store.record_pod("ns", "p", &[("app".to_string(), 10.0, 100.0)]);
        store.pods.get_mut("ns/p").unwrap().stale = true;
        store.record_pod(
            "ns",
            "p",
            &[
                ("app".to_string(), 30.0, 300.0),
                ("late".to_string(), 5.0, 50.0),
            ],
        );
        let pod = &store.pods["ns/p"];
        assert!(!pod.stale, "a fresh sample clears the stale flag");
        assert_eq!(pod.containers.len(), 2);
        assert_eq!(pod.containers[0].usage.cpu.max(), 30.0);
        assert_eq!(pod.containers[0].usage.cpu.avg(), 20.0);
        assert_eq!(pod.usage.cpu.last(), 35.0);
        assert_eq!(pod.key(), "ns/p");
    }

    #[test]
    fn retain_drops_pods_that_are_gone() {
        let mut store = MetricsStore::new(16);
        store.record_pod("ns", "a", &[]);
        store.record_pod("ns", "b", &[]);
        let keep: std::collections::HashSet<String> = ["ns/b".to_string()].into();
        store.retain_pods(&keep);
        assert_eq!(store.pods.keys().collect::<Vec<_>>(), vec!["ns/b"]);
    }

    #[test]
    fn cluster_totals_sum_the_newest_node_samples() {
        let mut store = MetricsStore::new(16);
        store.record_node("n1", 100.0, 1e9);
        store.record_node("n1", 200.0, 2e9);
        store.record_node("n2", 50.0, 1e9);
        store.nodes.get_mut("n1").unwrap().cpu_allocatable = 1000.0;
        store.nodes.get_mut("n2").unwrap().cpu_allocatable = 1000.0;
        store.nodes.get_mut("n1").unwrap().mem_allocatable = 4e9;
        let (cpu, cpu_cap, mem, mem_cap) = store.cluster_totals();
        assert_eq!((cpu, cpu_cap, mem, mem_cap), (250.0, 2000.0, 3e9, 4e9));
        assert_eq!(store.nodes["n1"].cpu_pct(), 20.0);
        assert_eq!(
            store.nodes["n2"].mem_pct(),
            0.0,
            "no allocatable, no percent"
        );
    }

    #[test]
    fn container_and_pod_summaries() {
        let mut store = MetricsStore::new(8);
        store.record_pod(
            "ns",
            "p",
            &[("a".to_string(), 50.0, 512.0), ("b".to_string(), 0.0, 0.0)],
        );
        let pod = store.pods.get_mut("ns/p").unwrap();
        pod.containers[0].ready = true;
        pod.containers[0].restarts = 2;
        pod.containers[1].restarts = 1;
        pod.containers[0].cpu_limit = 200.0;
        pod.containers[0].mem_limit = 1024.0;
        assert_eq!(pod.ready_string(), "1/2");
        assert_eq!(pod.restarts(), 3);
        assert_eq!(pod.containers[0].cpu_pct(), 25.0);
        assert_eq!(pod.containers[0].mem_pct(), 50.0);
        assert_eq!(pod.containers[1].cpu_pct(), 0.0);
    }
}
