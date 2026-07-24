# Contributing to kscope

Thanks for taking the time to help. Bug reports, documentation fixes and code
are all welcome.

## Ground rules

kscope is **read-only by design**. Pull requests that add mutating operations
(delete, scale, exec, port-forward, edit) will be declined, however well
implemented — the read-only guarantee is the feature. Everything else is on the
table.

## Getting started

```sh
git clone https://github.com/kscope-tui/kscope
cd kscope
cargo test              # unit tests, no cluster required
cargo run               # needs a reachable cluster
```

A local cluster is the easiest way to test against real data:

```sh
kind create cluster
kubectl apply -f https://github.com/kubernetes-sigs/metrics-server/releases/latest/download/components.yaml
kubectl patch -n kube-system deployment metrics-server --type=json \
  -p '[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--kubelet-insecure-tls"}]'
```

## Before you open a pull request

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

CI runs exactly these on Linux, macOS and Windows.

## Commit messages

Conventional Commits, because the changelog and release notes are generated
from them:

```
feat(logs): add JSON pretty-printing for structured lines
fix(metrics): handle nodes without an allocatable memory value
docs: explain the RBAC requirements
perf(logs): avoid re-styling lines outside the viewport
```

## Design notes worth knowing

* `src/app.rs` owns state; `src/ui/` only renders. Keep rendering side-effect
  free apart from reporting the viewport height.
* Anything on the log ingestion path is a hot path. New per-line work needs a
  benchmark or a good argument.
* Prefer graceful degradation over hard failure: a token that cannot list nodes
  should lose the node table, not the whole tool.

## Reporting bugs

Please include your kscope version, Kubernetes version, terminal emulator, and
the output of `KSCOPE_LOG=debug kscope --log-file /tmp/kscope.log` when relevant.
