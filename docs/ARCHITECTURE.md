# Architecture

kscope is a single binary with a library core. This document explains how the
pieces fit together and, more importantly, *why* — most of the design exists to
survive a pod that emits 50 000 log lines per second.

## Process shape

```
                    ┌──────────────────┐
  crossterm reader  │  input thread    │──┐   (blocking, dedicated OS thread)
                    └──────────────────┘  │
                    ┌──────────────────┐  │   mpsc
  metrics.k8s.io    │  metrics poller  │──┼──────────►  ┌───────────┐   ┌────────┐
                    └──────────────────┘  │             │    App    │──►│   ui   │
                    ┌──────────────────┐  │             │  (state)  │   │ render │
  pods/nodes list   │ inventory poller │──┤             └───────────┘   └────────┘
                    └──────────────────┘  │                  ▲
                    ┌──────────────────┐  │                  │ frame budget
  pods/log?follow   │ log streams × N  │──┘                  │ (max_fps)
                    └──────────────────┘
```

Everything is a tokio task except terminal input, which is blocking by nature
and therefore gets its own thread. Tasks never touch `App`; they only send
messages. `App` is single-threaded state, so there are no locks on the hot path.

## The main loop

`main::run` is a `select!` over four channels plus a frame ticker. Two rules
keep it responsive:

1. **`biased;`** — input is polled first, so the UI stays responsive even while
   log batches are pouring in.
2. **Redraw is decoupled from updates.** Handlers only set `app.dirty`. A frame
   is drawn when `dirty && last_draw.elapsed() >= frame_budget`. Ten thousand
   lines arriving in one tick cost exactly one redraw.

## Log pipeline

```
API server ──► kube log_stream ──► batching (512 lines / 100 ms) ──► mpsc
                                                                     │
                             LogLine::new: classify + timestamp scan ┘
                                        │
                                   LogBuffer (ring, fixed capacity)
                                        │
                          incremental view (filtered ids)
                                        │
                          Highlighter (visible window only)
```

Design choices worth defending:

* **Batching at the source.** The alternative — one channel message per line —
  turns a chatty container into a scheduler storm.
* **Classify once.** Severity is computed on arrival and stored as a one-byte
  enum. Filtering and colouring never re-scan text. Classification itself only
  looks at the first 512 bytes and uses a non-allocating case-insensitive scan
  with word-boundary checks, so `cherry` is not an `err`.
* **Unbounded by default, ring buffer on request.** Retention is a policy
  decision, not an implementation detail: `logs.buffer_lines = 0` (the default)
  keeps every line of the session so scrollback always reaches the container's
  first line, and a positive value swaps in eviction. The status bar reports
  resident bytes so the unbounded mode is never a silent surprise. The real
  ceiling is the kubelet's log rotation, which no client can see past.
* **Ring buffer with a global id space.** `lines[0]` has id `base`; evicting the
  front increments `base`. The filtered view stores ids, not indices, so
  eviction is O(1) instead of shifting every index.
* **Incremental view.** A new line is tested against the filter once and
  appended if it matches. Only a *filter change* triggers an O(n) rebuild. The
  integration suite asserts that the incremental view equals the rebuilt view.
* **Style lazily.** The highlighter runs on the `height` lines that are about to
  be drawn — never on the buffer. Cost per frame is independent of buffer size.

## Metrics pipeline

`metrics.k8s.io` is an aggregated API without types in `k8s-openapi`, so kscope
reads it as `DynamicObject` and decodes the JSON by hand. That is deliberate: it
keeps working if a cluster serves a different `v1betaX` revision.

Each poll produces a snapshot; `MetricsStore` appends one sample per series and
drops the oldest, so memory is bounded by `history × tracked resources`.

Three levels are tracked separately because in an incident you need all three:

| Level | Question it answers |
| --- | --- |
| node | Is the machine itself saturated? |
| pod | Is my workload the cause? |
| container | Which container — the app, or a sidecar? |

Requests and limits come from the *inventory* poller (pod specs), not from
metrics-server, and are joined onto the store in `reconcile_metrics_metadata`.
That join is what makes "62% of limit" possible rather than a bare number.

## State versus rendering

`src/app.rs` owns state and key handling. `src/ui/` is pure rendering with one
exception: the log view reports the viewport height back to `App`, because
"page down" is meaningless without knowing how tall a page is.

Key handling puts control chords in their own branch before plain keys — a
guard-based match silently makes `Char('n')` shadow `Ctrl-n`.

## Failure behaviour

kscope degrades instead of dying:

* No permission to list nodes or namespaces → those views empty out, pods still
  work.
* metrics-server absent → the metrics pane explains it; logs are unaffected.
* A log stream ends (container restart, connection drop) → automatic reconnect
  with capped exponential backoff, reported in the status bar.
* Panic → a hook restores the terminal before unwinding, so no broken shell.

## Read-only guarantee

Every Kubernetes call is `get`, `list`, `watch`, or `pods/log`. There is no code
path that constructs a `Patch`, `PostParams` or `DeleteParams`. This is a
maintained invariant, not an accident: pull requests adding mutation are out of
scope by policy.

## Testing strategy

* Unit tests live next to the code they cover (classification, quantity parsing,
  ring buffer, decoders).
* `tests/integration.rs` exercises the full log pipeline in both modes —
  250 000 lines through an unbounded buffer (asserting nothing is evicted and
  line zero is still reachable) and 200 000 lines through a 50 000 line ring
  buffer, plus filter, search and export and the metric store without a
  cluster.
* CI additionally runs a `kind` end-to-end job that starts a real pod, installs
  metrics-server, and asserts `kscope --dump` returns the expected lines.
