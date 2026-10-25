# Contributing to kscope

Thanks for taking the time to help. Bug reports, documentation fixes, ideas
and code are all welcome. You do not need to know Kubernetes internals or
ratatui to make a useful first change. Many of the
[good first issues](https://github.com/hknerts/kscope/issues?q=is%3Aopen+label%3A%22good+first+issue%22)
are small and self-contained.

Questions and half-formed ideas belong in
[Discussions](https://github.com/hknerts/kscope/discussions). Issues are for
bugs and concrete feature requests.

## The one rule

kscope is **read-only by design**. Pull requests that add mutating operations
(delete, scale, exec, port-forward, edit, apply) will be declined however well
they are implemented. The read-only guarantee is the feature, and it is what
lets people hand kscope to anyone. Everything else is on the table.

## From clone to running in five minutes

```sh
git clone https://github.com/hknerts/kscope
cd kscope
cargo test                  # ~120 tests, no cluster needed
make dev-up                 # 4 local kind clusters: stg-eu, stg-us, prod-eu, prod-us
cargo run -- -u stg         # browse both staging clusters as one
cargo run -- get pods -u prod
make dev-down               # when you are done
```

`make dev-up` ([`hack/dev-clusters.sh`](hack/dev-clusters.sh)) needs
[kind](https://kind.sigs.k8s.io/) and a container runtime it can use. It
installs metrics-server and seeds every cluster with deployments that log
(INFO and ERROR lines), and each staging cluster with a crash-looping pod. That
covers logs, metrics, triage (`!`) and fleets. It is idempotent: run it again to
reset the workloads.

### No cluster, or no kind

* **No cluster at all:** `cargo test` (or `make check`) needs none. State,
  key handling and rendering are all tested against a client that is never
  called, so most changes can be written and proven without a cluster.
* **minikube or k3d instead of kind:** any context works. For example
  `minikube start --addons=metrics-server` then `cargo run`, or
  `k3d cluster create dev` (k3s ships metrics-server) then `cargo run`. A
  fleet only needs two contexts with a shared word in their names, e.g.
  `minikube start -p stg-a` and `minikube start -p stg-b`, then
  `cargo run -- -u stg`.
* **Rust:** the toolchain comes from `rust-toolchain.toml` (stable, with
  rustfmt and clippy), so a plain [rustup](https://rustup.rs) install is
  enough. The minimum supported version is 1.89.

### Recording the demo

`make demo` builds a release binary and records `assets/demo.gif` from
[`hack/demo.tape`](hack/demo.tape) with [VHS](https://github.com/charmbracelet/vhs),
against the `make dev-up` clusters. Re-record it when a change alters what the
tape shows.

## Where things live

| Path | What it does |
| --- | --- |
| `src/main.rs` | Startup, the event loop, and every background request (listing, describe, discovery, context switch). |
| `src/app.rs` | All state and every key binding. Pure state transitions, with no I/O and no drawing. |
| `src/ui/` | Rendering only: one file per pane, `mod.rs` for the header and layout. |
| `src/k8s/` | Everything that talks to the API server: `logs.rs` streams, `discovery.rs` the watched inventory, `resources.rs` generic listing, `fleet.rs` multi-cluster. |
| `src/logs.rs` | The log ring buffer, filtering, level classification and highlighting. This is the hot path. |
| `src/describe.rs` | Object → YAML for `d`, including Secret redaction. |
| `docs/ARCHITECTURE.md` | Why it is built this way: process shape, pipelines, failure behaviour. |

## Writing tests

Pick the cheapest layer that proves the change:

* **Pure logic** (parsing, filtering, globs, classification): a plain
  `#[test]` next to the code. See `src/k8s/fleet.rs` or `src/logs.rs`.
* **Behaviour** (what a key press does, how state reacts to an API answer):
  build a real `App` with `app::tests::test_app`, which uses a client that is
  never called. Feed it events and assert on the state. See
  `opening_a_row_on_another_member_switches_there_first` in `src/app.rs`.
* **What is on screen**: draw a frame into ratatui's `TestBackend` and assert
  on the text. See `render_tests` in `src/ui/mod.rs`. Please add one for UI
  changes.
* **Against a cluster**: the `e2e` CI job runs kscope against kind. Add a step
  there for new non-interactive behaviour (`get`, `--dump`).

A test should fail without your change. Revert the fix once and watch it fail.

## Before you open a pull request

```sh
make check      # cargo fmt, clippy with warnings denied, and the test suite
```

CI runs the same on Linux, macOS and Windows. It also checks the minimum
supported Rust version (1.89), audits dependencies, builds the container image
for amd64 and arm64, and runs the end-to-end job against kind.

For UI changes, a screenshot in the pull request helps reviewers a lot.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/):

```
feat(logs): pretty-print JSON lines
fix(metrics): handle nodes without an allocatable memory value
docs: explain the RBAC requirements
perf(logs): avoid re-styling lines outside the viewport
```

Add a line to `CHANGELOG.md` under *Unreleased* for anything a user would
notice. The release page is built from that section.

## Design notes worth knowing

* `src/app.rs` owns state and `src/ui/` only renders. Keep rendering free of
  side effects apart from reporting the viewport height.
* Never block the event loop on the network. Requests are spawned, and their
  answers come back on a channel tagged with what was asked. An answer for
  something the user has since moved away from is dropped.
* Every one-shot request goes through `k8s::bounded`, so an API server that
  stops answering produces an error and never a hang. Watches and log streams
  reconnect with backoff instead.
* Degrade instead of failing. A token that cannot list nodes should lose the
  node table, not the whole tool. A fleet member that is down should be
  reported, not block the others.
* Anything on the log ingestion path runs per line, for up to hundreds of
  thousands of lines. New per-line work needs a measurement or a good argument.

## Releasing (maintainers)

1. Move *Unreleased* in `CHANGELOG.md` under a new `## [x.y.z] - date`
   heading, and bump `version` in `Cargo.toml`.
2. Commit as `release: x.y.z — summary`, tag `x.y.z` (no `v` prefix) and push
   the tag.
3. The release workflow writes the release page from that changelog section,
   attaches binaries for five targets with `.sha256` files, and pushes the
   container image to `ghcr.io/hknerts/kscope`.

## Reporting bugs

Please include your kscope version (`kscope --version`), Kubernetes version,
terminal emulator, and a debug log when it is relevant:

```sh
KSCOPE_LOG=debug kscope --log-file /tmp/kscope.log
```
