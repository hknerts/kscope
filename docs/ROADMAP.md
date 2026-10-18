# Roadmap

kscope's promise is narrow on purpose: **read-only, fast, safe to hand to
anyone.** Everything below is judged against that. A feature that needs a
mutating verb (`exec`, `port-forward`, `scale`, `delete`, `edit`) is out of
scope by policy, however useful. See [ARCHITECTURE.md](ARCHITECTURE.md) for
how the pieces fit.

Priorities: **P0** fixes something that breaks the promise, **P1** is the next
release, **P2** is worth doing after that, **P3** is an idea to validate first.

---

## Done in this round (performance)

| Area | Before | After |
| --- | --- | --- |
| Inventory | 5 sequential LISTs (pods, RS, nodes, ns, PVCs) every 5 s, full objects, capped at 2000 | One paginated LIST per kind, then WATCH; RS and namespaces metadata-only; managed fields and node image lists stripped |
| Events | LIST of up to 2000 events every 10 s | WATCH |
| Change latency | Up to a full refresh interval | ~250 ms debounce |
| Log reconnect | Replayed the container's whole history into the buffer; a finished container did so every 500 ms, forever | Resumes from the last delivered line via `sinceTime` + timestamp de-dup; empty streams back off |
| Log batching | Per-line 100 ms timeout: a 20 lines/s pod was held back until 512 lines (~25 s) | Deadline from the batch's first line |
| Invalid UTF-8 | Tore the stream down (then replayed, see above) | Lossy decode |
| Active search | Full buffer rescan per log batch (5.2 ms per batch at 250k lines) | Incremental (0.2 ms per 512-line batch) |
| Status bar memory | Walk of every line per frame (330 µs at 250k lines) | Counter (2 ns) |
| Tables | Every row built every frame | Only the visible window |
| Discovery | One request per API group, sequentially | Aggregated discovery (2 requests), fallback for < 1.26 |
| Startup / context switch | Blocked the UI on version probe and client build (exec auth plugins) | Off the UI task |
| Idle | 30 wakeups/s | Frames only when dirty; 1 s housekeeping tick |
| Browsed list | Never refreshed after the first listing; capped at 1000; stale answers overwrote a newer kind | Refreshed while browsing, paged to 50 000, answers tagged and stale ones dropped |

---

## P0 — keep the promise

1. ~~**Redact Secrets in describe.**~~ Done in 1.1.0: values render as
   `<redacted, N bytes>`, opt-in reveal via `describe.show_secret_values`. The
   `last-applied-configuration` annotation, which would embed the values, was
   already stripped for every kind.
2. **Mouse capture off by default.** With capture on, the terminal cannot
   select text, so nobody can copy a log line or an error message out of
   kscope — the most common thing you want to do with it. Make it opt-in, or
   toggle with a key.
3. **`s` (save) writes into the current directory.** Running kscope from a
   repository litters it with `kscope-*.log` files (this checkout had 11).
   Write to a configurable directory (default: the platform data dir), and
   show the full path in the status bar.

## P1 — next release

### Browsing
4. **Server-side table columns.** Ask the API server for the `Table` format
   (`Accept: application/json;as=Table;g=meta.k8s.io;v=v1`). That returns
   exactly the columns `kubectl get` prints for *every* kind — including CRD
   `additionalPrinterColumns` — so pods get READY / RESTARTS / NODE / IP,
   Services get TYPE / CLUSTER-IP / PORTS, and nothing has to be hand-coded.
   Keep `summarise` for the health flag that drives `!`.
5. **Watch the browsed kind** with a `DynamicObject` reflector instead of the
   periodic re-list, completing the move to watches. Keeps the row list live
   in the detail view too, which today is frozen to protect the selection.
6. **Sort the list** by any column (`S` already cycles sorts in monitoring;
   reuse it), with restarts and age as the useful defaults for triage.
7. **CronJob → Job → Pod** resolution so opening a CronJob streams its latest
   run. Today it resolves to nothing.

### Logs
8. **JSON logs.** Detect structured lines once, on arrival (same place level
   classification happens), then let `/` and `\` filter on fields
   (`level=error`, `user_id=42`) and offer a compact `msg`-only view.
9. **Multi-line grouping.** Fold stack traces into the line that started them
   so filtering by level keeps the trace attached to its error.
10. **Jump by time** (`T` → `12:04`, `-5m`) using the timestamps every line now
    carries internally.

### Quality
11. **Fix known small bugs found during this review:**
    * `search.current` points at the wrong line after ring-buffer eviction.
    * The browse header lists `Enter` twice ("open" and "open logs").
    * A context switch always lands on the new context's default namespace,
      even when started with `-A`.
12. **Guard performance in CI.** Criterion benches for ingest, filter rebuild
    and frame render at 250k lines; a ratatui `TestBackend` snapshot test per
    view so layout regressions show up in review.

## P2 — after that

13. **Pulse view** (`:pulse`): one screen of everything in trouble cluster-wide
    — crash-looping and pending pods, NotReady nodes, PVCs over 85 %, the last
    hour of Warning events grouped by reason. The question on-call opens a tool
    to answer, answered without navigating.
14. **Relationships.** From any object, show its owner chain and dependants
    (Deployment → ReplicaSets → Pods → Node; Service → EndpointSlices →
    Pods; PVC → PV → StorageClass) and jump along them.
15. **Rollout history diff.** For a Deployment, diff the pod templates of its
    ReplicaSet revisions — "what changed in the deploy that broke it", read
    only.
16. **Events timeline** for a workload, merged across its pods and the
    workload itself, aligned with the log timeline.
17. **`--dump` for workloads** (`--dump deploy/checkout`) with `--json`
    output, so scripts get the same pod resolution the TUI uses.
18. **Distribution:** Homebrew tap, `kubectl krew` plugin (`kubectl kscope`),
    Scoop, Nix. Installation is currently curl or cargo only.

## P3 — validate before building

19. **Optional Prometheus backend** for metrics history longer than the
    in-memory window (read-only queries, opt-in, URL in config).
20. **Cross-context compare:** the same workload side by side in two clusters
    (staging vs prod image, replicas, resource limits).
21. **Configurable key bindings and light-terminal themes.**

---

## Candidates to remove or slim down

| Item | Why | Proposal |
| --- | --- | --- |
| Five-row header with a permanent shortcut grid and ASCII binoculars | Costs 6 of ~40 rows on a laptop terminal, and the binoculars take 17 columns | One-line header by default, full grid on `?`; drop the icon below ~160 columns |
| `inventory_refresh_ms` / `events_refresh_ms` | With watches they are only heartbeats; the names now mislead | Deprecate in favour of one `general.refresh_ms`, keep the old keys as aliases for a release |
| `c` (clear buffer) and `x` (detach all) | Rarely used, easy to hit by accident in a log view; `x` leaves an empty pane with no obvious way back | Fold into a confirmation, or drop `x` — `Esc` and re-open does the same |
| Hard-coded short-name table in `resources.rs` | Discovery already serves `shortNames`; the hand list goes stale and misses CRD short names | Read them from aggregated discovery once kube exposes them, or from the raw discovery response |

## Explicitly out of scope

`exec`, `port-forward`, `attach`, `scale`, `delete`, `edit`, `apply`, cordon /
drain, and anything else that needs a verb other than `get`, `list` or
`watch`. They are what [k9s](https://github.com/derailed/k9s) is for.
