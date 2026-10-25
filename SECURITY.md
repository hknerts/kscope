# Security Policy

## Supported versions

The latest minor release receives security fixes.

| Version | Supported |
| ------- | --------- |
| 1.7.x   | ✅        |
| < 1.7   | ❌        |

## Reporting a vulnerability

Please **do not** open a public issue. Report privately through
[GitHub Security Advisories](https://github.com/hknerts/kscope/security/advisories/new)
("Report a vulnerability" on the Security tab).

Expect an acknowledgement within 3 working days and an assessment within 10.
We will credit you in the advisory unless you prefer otherwise.

## Threat model

kscope reads cluster data with the credentials it is given and never mutates
cluster state. Relevant risk areas:

* **Credential handling** — kscope uses the standard kubeconfig or in-cluster
  service account. It never writes credentials to disk.
* **Untrusted log content** — log lines are attacker-influenced data. They are
  rendered as text with escape sequences neutralised by the TUI backend, but
  reports of terminal escape injection are taken seriously.
* **Exported files** — `s` writes the visible buffer to the working directory
  with default permissions. Do not export logs to a shared directory.
* **Dependencies** — `cargo deny` and Dependabot run in CI.
