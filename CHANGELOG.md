# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Logs from every cluster of a fleet.** `Enter` (or `l`) on a workload in
  a fleet streams its pods on every member that has it, merged into one
  timeline with the cluster at the start of each line. No context switch.
  Rollouts on any member are followed within 15 seconds.
- **Compare across a fleet (`a`).** The open object as each member has it:
  status, replicas, images, and whether the spec matches. Whatever differs
  from most members is flagged, down to the field
  (`spec differs on stg-us (template.spec.containers[0].image)`). Members
  without the object, or unreachable, are said to be.
- **Live CPU and memory in the list.** Pods show their own usage;
  Deployments, StatefulSets, DaemonSets, Jobs and Services the sum over the
  pods they select. `-` without metrics-server. `kscope get` prints `CPU` and
  `MEMORY` columns too.
- **Images in the list**, as `name:version` without the registry
  (`registry.k8s.io/coredns/coredns:v1.14.2` → `coredns:v1.14.2`), in the
  TUI and in `kscope get`.
- **`v` reveals a Secret's values, decoded**, in describe, and hides them
  again. Binary values are named, not printed. `describe.allow_reveal = false`
  turns it off for a team.

### Changed

- The logs title says what is merged: `3 pods merged`, or across a fleet
  `4 pods merged from 2 of 2 clusters`. The empty logs pane explains that a
  workload or Service merges all of its pods.

## [1.6.2] - 2026-10-21

### Fixed

- **Describe is a tab, not a window.** `l`, `m` and `e` now lead straight on
  from describe to logs, metrics and events. `d` from the list opens the
  detail pane on the describe tab, and `Esc` goes back to the list as from
  any tab.

## [1.6.1] - 2026-10-21

### Changed

- **Fleets have a box of their own.** The left pane shows a `fleets` box
  above the `contexts` box instead of one mixed list. One cursor runs over
  both: `↑`/`↓` (or `j`/`k`) move from the last fleet onto the first context
  with no `Tab`. The box holding the cursor is the highlighted one.
- **Describe opens in the same window.** `d` draws the YAML in the right-hand
  pane, where the list or the open tab was, instead of a floating window over
  the whole screen. The contexts, header and status bar stay in view.

## [1.6.0] - 2026-10-21

### Added

- **Built-in `stg` and `prod` fleets.** `stg` matches contexts named
  `*stg*` or `*staging*`, `prod` matches `*prod*`, `*prd*` or `*pro*`. They appear in
  the contexts pane whenever a context matches, with no config file.
- **Fleets from the environment.** `KSCOPE_FLEET_<NAME>="glob,glob"` defines
  a fleet or overrides one. The environment wins over `[fleets]` in the
  config file, which wins over the built-ins. An empty value or `[]` switches
  a fleet off.

### Documentation

- The README explains how to set fleets up (built-ins, config file,
  environment and their order) and compares fleets with k9s, which shows one
  context at a time.

## [1.5.0] - 2026-10-21

### Changed

- **A calmer, aligned header.** The left side says where you are in labelled
  rows (`context`, `showing pods in shop · only problems`, `cpu`/`mem`,
  `health` with items, problems and warnings). The middle lists only the keys
  that work on the current screen, aligned per column. The mixed strip of
  tabs and modes is gone.
- **`e` opens events**, like `l` logs and `m` metrics. `E` still works.
  The separate "errors only" key is gone: `L` steps the minimum level to
  ERROR, which is the same filter, and now says what it shows.
- The detail tabs read `l logs  m metrics  e events  d describe`, and the
  view is called "metrics" everywhere, not "monitoring" in some places.
- `!` is "only problems" everywhere (header, help, status), and the status
  says how to show everything again.
- The status bar keeps messages on the left and right-aligns only the facts
  that say something: no `drop:0`, and no filter when nothing is filtered.

### Fixed

- DEBUG and TRACE lines were hidden from the start: the log filter's default
  threshold came from `Level`'s default, which is INFO. Every level is shown
  until `L` narrows it.

## [1.4.0] - 2026-10-21

### Fixed

- **An API server that stops answering no longer freezes kscope.** kube's
  client has no read timeout, so a listing, describe, discovery or version
  probe against a server that accepted the connection and then went silent
  waited forever. Because listings never overlap, the browsed list then
  stopped refreshing for the rest of the session. One-shot requests now fail
  after 30 s with "no answer from the API server". A context switch has 120 s,
  which leaves time for an SSO login in an exec credential plugin.
- Discovery is retried with backoff when it fails, instead of leaving an empty
  `:` palette, and nothing could be listed, until the context was re-selected.
  A late catalogue from a context you already left is ignored.
- With a capped log buffer, evicting old lines no longer moves the search
  cursor (`n`/`N`) and the scroll position onto different lines.
- Switching context keeps "all namespaces" when kscope was started with `-A`.
- `kscope -u stg -n shop` lists only `shop`. A fleet given on the command
  line used to switch to all namespaces regardless of `-n`.
- Opening a row from another fleet member said "no pods behind this
  deployment" while the new cluster's pods were still loading. It now says
  "waiting for the pod inventory…". The contexts pane cursor also follows the
  switch.

### Security

- Update `rustls-webpki` to 0.103.16 and `webpki-root-certs` to 1.0.10 for
  RUSTSEC-2026-0382 and RUSTSEC-2026-0383: certificate paths through the
  distrusted emSign intermediates are rejected.

### Changed

- A fleet's list is refreshed every 15 s instead of every 5 s. Each refresh is
  a full LIST on every member.
- Release pages are written from this changelog, and every archive comes with
  a `.sha256`. The container image cross-compiles arm64 instead of emulating
  it, so the image is ready minutes after a tag instead of half an hour.

- README: the config file lives in the platform's config directory, which
  on macOS is `~/Library/Application Support/io.kscope.kscope/`, not
  `~/.config/kscope/`.

## [1.3.1] - 2026-10-19

### Changed

- The header shows what kscope can do and what is live, in the same five
  rows. A strip of capability chips (logs, metrics, events, describe, fleet,
  triage) lights up for whatever is on screen, next to a permanent read-only
  mark. Cluster CPU and memory are gauges coloured by the metrics thresholds,
  and a counts line shows items, how many are in trouble, warning events and
  nodes. The context's dot turns green once the apiserver has answered. The
  logo is a pair of binoculars with glass lenses rather than spectacles. On
  a narrow terminal, chips and shortcut columns that do not fit whole are
  left out instead of being cut mid-word.

- A cluster without metrics-server now says so in plain words ("no metrics:
  metrics-server is not installed") instead of a raw, truncated 404 parse
  error. A 403 and a 503 are explained the same way.

## [1.3.0] - 2026-10-19

### Added

- `kscope get KIND [NAME] -u FLEET`: kubectl-style listing across a fleet,
  e.g. `kscope get pods -u stg`, `kscope get deploy checkout -u prod`. `-n`,
  `-l`, `-A` and `--context` work before or after the subcommand.
- `-u` is short for `--fleet`, and a plain word selects every context whose
  name contains it (`-u stg`), so no config is needed. An exact context name
  still means just that context, and a configured fleet name still wins.

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
