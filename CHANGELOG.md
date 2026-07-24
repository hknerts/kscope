# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Logs are now unlimited by default. Attaching requests the container's entire
  retained history instead of a 500-line tail, and the in-memory buffer is
  unbounded (`logs.buffer_lines = 0`) instead of a 50 000-line ring buffer.
  Set `tail_lines` or `buffer_lines` to a positive value to restore a cap.

### Added

- `--since SECONDS` to fetch only recent lines.
- Live memory usage of the retained buffer in the status bar.

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

[Unreleased]: https://github.com/kscope-tui/kscope/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kscope-tui/kscope/releases/tag/v0.1.0
