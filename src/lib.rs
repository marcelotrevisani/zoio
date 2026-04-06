//! zoio - Cross-platform process resource monitor
//!
//! This library exposes the core monitoring primitives used by the `zoio`
//! binary: process discovery, regex-based filtering, per-process metrics
//! sampling, in-memory history, and CSV export.

pub mod app;
pub mod csv_export;
pub mod metrics;
pub mod process_filter;
pub mod process_monitor;

pub use csv_export::export_to_csv;
pub use metrics::{MetricKind, MetricSample, ProcessHistory, ProcessIdentity};
pub use process_filter::{FilterSet, ProcessFilter};
pub use process_monitor::{MonitorConfig, ProcessMonitor, SampledProcess};
