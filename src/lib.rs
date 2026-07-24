//! kscope — a read-only Kubernetes TUI for logs and live metrics.
//!
//! The crate is split into a library and a thin binary so that the log
//! pipeline, the metric store and the Kubernetes layer can be tested (and
//! reused) without a terminal or a cluster.
//!
//! * [`logs`] — ring buffer, severity classification, filtering, search,
//!   highlighting.
//! * [`metrics`] — quantity parsing and bounded time series.
//! * [`k8s`] — client bootstrap, inventory polling, log streaming, metrics
//!   polling. Every call is a `get`/`list`/`watch`; nothing mutates.
//! * [`app`] — state and key handling. [`ui`] — rendering.

pub mod app;
pub mod config;
pub mod event;
pub mod k8s;
pub mod logs;
pub mod metrics;
pub mod ui;
