# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.2.0] - 2026-10-19

### Added

- **Fleets: browse many clusters as one.** Name sets of contexts by glob in
  `[fleets]` (`staging = ["stg-*"]`), or pass globs directly with
  `--fleet 'stg-*'` or `/` on the contexts pane. Every listing then spans all
  members, with a `context` column. `/`, `!`, `L` and `:` work across them,
  and opening a row from another member switches to that cluster first.
  Members are queried concurrently, each with a 10 s budget, and an
  unreachable member is reported without blocking the others.
- `--find TEXT` prints every object of `--kind` whose name or namespace
  contains TEXT, across the fleet or the current context, and exits 1 when
  nothing matches.
- `-k`/`--kind` picks the resource type to start on.

### Fixed

- A context switch could leave the previous cluster's describe overlay on
  screen for an object with the same name.
- The browse header listed `Enter` twice.

## [1.1.1] - 2026-10-18

### Security

- Update `rustls` to 0.23.46 and `rustls-webpki` to 0.103.15 for
  RUSTSEC-2026-0285, RUSTSEC-2026-0346, RUSTSEC-2026-0348 and
  RUSTSEC-2026-0349. Every connection kscope makes to the API server goes
  through rustls; 1.1.0 binaries were built with the affected version, so
  upgrade.

## [1.1.0] - 2026-10-18

### Security

- **Secret values are redacted in describe.** `d` on a Secret shows each key
  as `<redacted, N bytes>` instead of its base64 value, for both `data` and
  `stringData`. Opt back in with `describe.show_secret_values = true`.

### Performance

- The inventory (pods, ReplicaSets, nodes, namespaces, PVCs) and events are
  **watched** instead of re-listed every few seconds. The API server sees one
  paginated LIST per kind and then only changes; the UI sees changes within
  ~250 ms instead of up to a full refresh interval.
- Discovery uses the aggregated discovery API (2 requests instead of one per
  API group, sequentially), with a fallback for clusters older than 1.26.
- Startup and context switches no longer block the UI on the apiserver version
  probe or on building the new client.
- The UI no longer wakes 30 times a second while idle; frames are scheduled
  only when something changed.
- An active search no longer rescans the whole buffer on every log batch, the
  buffer's memory figure is a counter instead of a per-frame walk, and the
  resource, events and describe views build only the rows on screen.

### Fixed

- A log stream that reconnected re-downloaded the container's entire history
  into the buffer, duplicating it. For a finished container this happened
  every 500 ms, forever. Streams now resume from the last delivered line.
- Log lines arriving faster than one per 100 ms were held back until 512 had
  piled up, which could delay a moderately chatty pod by tens of seconds.
- A line with invalid UTF-8 tore the log stream down (and triggered the replay
  above); it is now decoded lossily.
- The browsed resource list never refreshed after the first listing; it is now
  kept live while browsing, and paged past the old 1000-item cap.
- A listing that finished after the resource type was changed overwrote the
  new list and left it stuck.

### Changed

- **Detail views are switched by letter, not number**: `Enter` or `l` for logs,
  `m` for monitoring, `E` for events, `d` for the describe overlay. The label
  selector moved from `l` to `L` while browsing, where it no longer collides.
- **Monitoring is scoped to the object you opened** rather than showing
  cluster-wide tables. A Deployment totals its replicas and lists them, a pod
  breaks down by container, a node shows its own load plus what is scheduled on
  it, a claim shows its fullness. `m` no longer cycles node/pod/volume tables,
  because there is nothing to cycle.
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

- **Describe** (`d`): renders any object as YAML, CRDs included, with
  managed fields, resource versions and the `last-applied-configuration`
  annotation stripped. The YAML emitter is written in-tree rather than pulling
  in the unmaintained `serde_yaml`. It is an overlay rather than a tab, and
  works straight from the list without opening the object first.
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

[Unreleased]: https://github.com/hknerts/kscope/compare/1.1.1...HEAD
[1.1.1]: https://github.com/hknerts/kscope/releases/tag/1.1.1
[1.1.0]: https://github.com/hknerts/kscope/releases/tag/1.1.0
[0.1.0]: https://github.com/hknerts/kscope/releases/tag/v0.1.0
