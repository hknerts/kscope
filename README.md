# kscope

**A fast, read-only Kubernetes TUI for logs and live metrics.**

kscope does two things and tries to do them very well: it shows you container
logs, and it shows you what your cluster is actually consuming. It is not a
replacement for `k9s` — there is no editing, scaling, deleting or shelling in.
That is the point: kscope is safe to hand to anyone with a view-only token, and
it stays fast because it never has to be anything else.

```
┌ kscope  1:logs  2:metrics   ns:prod  pods:184  nodes:12  cluster:cpu 61% mem 74% ┐
│ pods ─────────────┐ levels: FATAL 0  ERROR 42  WARN 118  INFO 9k  DEBUG 2k       │
│ ▾ checkout-7d9c   │┌ logs · prod/checkout-7d9c:app · follow · nowrap ───────────┐│
│    ● app     ↺0   ││ 10:22:31.004 INFO  order 8812 accepted in 41ms             ││
│    ○ envoy   ↺2   ││ 10:22:31.219 WARN  retrying payments upstream (attempt 2)  ││
│ ▸ payments-5f2a   ││ 10:22:31.402 ERROR connect 10.4.2.11:8443 deadline exceeded││
└───────────────────┘└────────────────────────────────────────────────────────────┘
 streaming prod/checkout-7d9c:app │ 11482/50000 lines  filter:>=WARN  FOLLOW │ ? help
```

## Why

Reading logs through `kubectl logs -f` works until you need to page back, grep,
follow three containers at once, or notice that the pod you are debugging is
being OOM-throttled. kscope keeps all of that on one screen:

* **Logs** — paging, regex search with match highlighting, level filtering,
  automatic error highlighting, multi-container streaming, export to file.
* **Metrics** — live CPU and memory at **node**, **pod** *and* **container**
  granularity, with usage-versus-limit percentages and rolling sparklines.

## Install

### From source

```sh
cargo install --locked --git https://github.com/kscope-tui/kscope
```

### Release binaries

Static binaries for Linux (x86_64, aarch64), macOS (Intel, Apple Silicon) and
Windows are attached to every [release](https://github.com/kscope-tui/kscope/releases).

```sh
curl -sSfL https://github.com/kscope-tui/kscope/releases/latest/download/kscope-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo install kscope /usr/local/bin/
```

### Container

```sh
docker run --rm -it -v ~/.kube:/home/nonroot/.kube:ro ghcr.io/kscope-tui/kscope:latest
```

## Usage

```sh
kscope                        # current context, current namespace
                              # (full history from container start, nothing evicted)
kscope -n payments            # a specific namespace
kscope -A                     # every namespace you can read
kscope --context staging      # a specific kubeconfig context
kscope --tail 2000            # only the last 2000 lines instead of everything
kscope --buffer 200000        # cap memory at 200k lines (ring buffer)
kscope --since 3600           # only lines from the last hour
kscope --dump checkout-7d9c:app > app.log   # non-interactive, for scripts
```

## Key bindings

| Key | Action |
| --- | --- |
| `q`, `Esc`, `Ctrl-c` | quit |
| `?` | help overlay |
| `1` / `2` | logs view / metrics view |
| `Tab` | move focus between sidebar and content |
| `Enter` | expand a pod / attach a container |
| `a` / `x` | attach every container / detach everything |
| `j` `k` `↑` `↓` | move one line |
| `Ctrl-d` / `Ctrl-u` | half page down / up |
| `Ctrl-f` / `Ctrl-b`, `PgDn` / `PgUp` | full page down / up |
| `g` / `G` | top / bottom (bottom re-enables follow) |
| `h` `l` `←` `→` | horizontal scroll when wrapping is off |
| `/` then `n` / `N` | regex search, next / previous match |
| `\` | filter lines; prefix with `!` to exclude |
| `L` / `e` | cycle minimum level / errors only |
| `F` / `w` | toggle follow / wrapping |
| `t` / `p` | toggle timestamps / previous container logs |
| `c` / `s` | clear buffer / save visible buffer to a file |
| `Ctrl-n` / `Ctrl-p` | change namespace / filter the pod list |
| `m` / `S` | switch metric table / cycle sort order |

## How far back can I scroll?

By default, **all the way to the container's first line**. There are no limits
on kscope's side:

* On attach, kscope asks the API server for the container's entire retained
  history (`tail_lines` unset), not a fixed tail.
* The in-memory buffer is **unbounded** by default (`logs.buffer_lines = 0`), so
  nothing is evicted for the lifetime of the session. `g` jumps to the first
  line, always.
* The status bar shows the live line count and the memory those lines occupy, so
  an unbounded session cannot grow silently.

Two limits are outside kscope's control, and it is worth knowing them:

1. **Kubelet log rotation.** The API server can only return what is still on the
   node. Kubelet rotates container logs at `containerLogMaxSize` (10 MiB by
   default) and keeps `containerLogMaxFiles` (5 by default) of them. Anything
   older is gone before any tool can read it — `kubectl logs` has the same
   ceiling. For genuinely permanent history you need a log shipper.
2. **Container restarts.** A restart starts a fresh log. Press `p` to read the
   previous, crashed instance instead.

If you would rather trade history for a memory ceiling, set a cap:

```toml
[logs]
buffer_lines = 200000   # ring buffer; oldest lines are evicted
tail_lines = 5000       # only fetch the last 5000 lines on attach
```

## Configuration

kscope runs with zero configuration. To customise it, drop a file at
`~/.config/kscope/config.toml` (see [`examples/config.toml`](examples/config.toml)):

```toml
[logs]
buffer_lines = 0     # 0 = unlimited (default): never evict
tail_lines = 0       # 0 = everything the API server still has (default)
smart_case = true

[metrics]
refresh_ms = 5000
history = 240
warn_pct = 75.0
critical_pct = 90.0

[[highlight]]
pattern = 'order_id=[0-9a-f]+'
fg = "#ff8800"
bold = true
```

## Permissions

kscope only ever performs `get`, `list` and `watch`. A sufficient role:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: kscope-viewer
rules:
  - apiGroups: [""]
    resources: ["pods", "pods/log", "nodes", "namespaces"]
    verbs: ["get", "list", "watch"]
  - apiGroups: ["metrics.k8s.io"]
    resources: ["pods", "nodes"]
    verbs: ["get", "list"]
```

Node metrics and the namespace list are optional: kscope degrades gracefully
when a namespaced token cannot read them.

## Performance notes

The design targets pods that emit tens of thousands of lines per second:

* Log lines arrive in **batches** (512 lines or 100 ms) so a chatty container
  cannot livelock the render loop.
* Lines are classified into a severity level exactly once, on arrival, and
  stored in an unbounded deque — or a fixed-capacity ring buffer if you set
  `logs.buffer_lines`.
* The filtered view is a list of line ids maintained **incrementally**; only a
  filter change costs a full pass.
* Redraws are capped at `general.max_fps` (30 by default) and only happen when
  something actually changed.
* **Only the visible window is styled** — cost per frame is independent of how
  many lines are in the buffer.
* Release builds use fat LTO and a single codegen unit.

## Requirements

* A reachable cluster (kubeconfig or in-cluster service account).
* [metrics-server](https://github.com/kubernetes-sigs/metrics-server) for the
  metrics view. Without it, the logs view still works and the metrics pane
  explains what is missing.

## Contributing

Bug reports, feature requests and pull requests are welcome — see
[CONTRIBUTING.md](CONTRIBUTING.md). Everyone participating is expected to follow
the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Released under the [MIT License](LICENSE).
