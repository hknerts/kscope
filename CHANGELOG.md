# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Reworked the interface into two panes.** The left pane lists kubeconfig
  contexts; the right pane browses a resource type and opens one object into
  logs, metrics and events tabs. The old pod sidebar is gone, along with its
  `a` (attach all) and `Ctrl-p` (filter pods) bindings — the list filter is now
  `/`, and opening an object attaches its logs.
- `Esc` now steps out of the detail view instead of quitting outright; use `q`
  or `Ctrl-c` to quit.
- Logs are now unlimited by default. Attaching requests the container's entire
  retained history instead of a 500-line tail, and the in-memory buffer is
  unbounded (`logs.buffer_lines = 0`) instead of a 50 000-line ring buffer.
  Set `tail_lines` or `buffer_lines` to a positive value to restore a cap.

### Added

- **Describe tab** (`4`): renders any object as YAML, CRDs included, with
  managed fields, resource versions and the `last-applied-configuration`
  annotation stripped. The YAML emitter is written in-tree rather than pulling
  in the unmaintained `serde_yaml`.
- **Live PersistentVolumeClaim usage** in the metrics view, read from kubelet's
  `stats/summary`. The Kubernetes API only knows the *requested* size; this is
  how full the volume actually is. Needs `nodes/proxy`, and only exists for
  volume plugins that report metrics — `hostPath` and `local-path` do not, and
  the panel says so instead of showing a bare `-`.
- **Workload-level log streaming.** Opening a Deployment, StatefulSet,
  DaemonSet, Job or ReplicaSet merges every replica into one timeline; a Service
  or CRD resolves through its `spec.selector`; a Namespace or Node streams
  everything running there. An object with no pods behind it says so rather than
  silently attaching to nothing.
- **Dynamic stream membership.** The pod set is re-derived on every inventory
  refresh, so a rollout hands the stream over to the new pods instead of going
  quiet on the old ones. Surviving streams are left untouched, so history is not
  replayed on every tick.
- `logs.max_streams` (default 50) caps concurrent log streams, so opening a
  large DaemonSet cannot open hundreds of watches at once.
- **Triage filter** `!`: narrow any listing to the objects in trouble. Pod
  status now reads container state rather than phase, so a pod stuck in
  `CrashLoopBackOff` or `ImagePullBackOff` is reported as such instead of as
  `Running`.
- **Label selectors**: `-l`/`--selector` and the `l` key. Applied server-side to
  listings and used to narrow which pods a workload's logs come from. Supports
  `k=v`, `k==v`, `k!=v`, `k` and `!k`.
- **Node journals**: opening a Node shows its kubelet logs, with `v` cycling
  kubelet / containerd / kernel. Needs `nodes/proxy` and the `NodeLogQuery`
  feature gate; when either is missing the error says which.
- **`:` resource palette** with k9s-style autocompletion. Candidates come from
  the cluster's discovery API, so CRDs complete like built-in kinds, and
  kubectl short names (`po`, `deploy`, `pvc`, …), singular kinds and fuzzy input
  all resolve.
- **Events tab** (`3`), scoped to the object you have open, with a warnings-only
  toggle (`W`).
- **Live context switching**: `Enter` on a context rebuilds the client and
  restarts every poller without restarting kscope.
- `--since SECONDS` to fetch only recent lines.
- `Ctrl-r` to re-list the current resource type.

### Fixed

- Node journals no longer present kubelet's `/var/log` directory index as if it
  were log output. With the `NodeLogQuery` feature gate disabled the endpoint
  ignores the query and serves a browsable HTML index with a 200; that is now
  detected and reported as the missing feature gate it is.
- Changing the namespace scope (`Ctrl-n`) now restarts the inventory, metrics
  and event pollers, which were previously left bound to the old namespace.
- `kube` 4.2 moved the rustls crypto provider behind a feature flag; without it
  every run panicked at client construction.

## [0.1.0] - 2026-07-24

### Added

- Log viewer: multi-container streaming, paging, follow mode, line wrapping and
  horizontal scrolling.
- Regex search with smart case, match highlighting and next/previous navigation.
- Line filtering by include/exclude regex, minimum severity level, and an
  errors-only toggle.
- Automatic severity classification and colouring, plus configurable regex
  highlight rules.
- Buffer export to a timestamped file.
- Metrics viewer: cluster gauges, node table, pod table and per-container
  breakdown with rolling sparklines and usage-versus-limit percentages.
- Namespace scoping, pod list filtering and sortable metric tables.
- Non-interactive `--dump` mode for scripts.
- Configuration file with theme and highlight customisation.

[Unreleased]: https://github.com/hknerts/kscope/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/hknerts/kscope/releases/tag/v0.1.0
