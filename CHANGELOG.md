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
