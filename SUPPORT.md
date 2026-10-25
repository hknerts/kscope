# Getting help

* **How do I…? / Is this expected?** Ask in
  [Discussions](https://github.com/hknerts/kscope/discussions). Half-formed
  questions are welcome there.
* **Something is broken.** Open a
  [bug report](https://github.com/hknerts/kscope/issues/new?template=bug_report.yml)
  with `kscope --version`, your Kubernetes version and terminal, and a debug
  log when it helps:

  ```sh
  KSCOPE_LOG=debug kscope --log-file /tmp/kscope.log
  ```

* **An idea for a feature.** Open a
  [feature request](https://github.com/hknerts/kscope/issues/new?template=feature_request.yml).
  Remember that kscope stays read-only: anything that needs `exec`,
  `port-forward`, `scale`, `delete` or `edit` is out of scope.
* **A security problem.** Do not open an issue; see [SECURITY.md](SECURITY.md).

Before asking, the [README](README.md) covers key bindings, fleets,
configuration and the RBAC kscope needs, and `?` inside kscope shows every key.

kscope is maintained by volunteers, so there is no guaranteed response time.
