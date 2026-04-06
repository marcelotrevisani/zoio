//! Metric data structures used to record per-process resource usage over time.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// The four resource dimensions that `zoio` tracks per process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MetricKind {
    /// CPU utilization as a percentage across all cores (0.0 .. 100.0 * ncpu).
    Cpu,
    /// Resident memory in bytes.
    Memory,
    /// Cumulative disk I/O (read + write) in bytes per sample interval.
    Disk,
    /// Cumulative network I/O (rx + tx) in bytes per sample interval.
    Network,
}

impl MetricKind {
    pub const ALL: [MetricKind; 4] = [
        MetricKind::Cpu,
        MetricKind::Memory,
        MetricKind::Disk,
        MetricKind::Network,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MetricKind::Cpu => "CPU (%)",
            MetricKind::Memory => "Memory (MiB)",
            MetricKind::Disk => "Disk I/O (KiB/s)",
            MetricKind::Network => "Network I/O (KiB/s)",
        }
    }

    pub fn short_name(self) -> &'static str {
        match self {
            MetricKind::Cpu => "cpu",
            MetricKind::Memory => "memory",
            MetricKind::Disk => "disk",
            MetricKind::Network => "network",
        }
    }
}

/// Stable identity of a tracked process.
///
/// The `pid` can be recycled by the OS, so we combine it with the executable
/// name and the start time (seconds since UNIX epoch) to get an identity we
/// can de-duplicate across samples even as processes come and go.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub name: String,
    pub start_time: u64,
    pub parent_pid: Option<u32>,
}

impl ProcessIdentity {
    pub fn new(pid: u32, name: impl Into<String>, start_time: u64) -> Self {
        Self {
            pid,
            name: name.into(),
            start_time,
            parent_pid: None,
        }
    }

    pub fn with_parent(mut self, parent_pid: Option<u32>) -> Self {
        self.parent_pid = parent_pid;
        self
    }

    /// A short human-readable label used in the UI legend.
    pub fn display_label(&self) -> String {
        format!("{} (pid {})", self.name, self.pid)
    }
}

/// A single point in a metric time-series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MetricSample {
    /// Seconds since monitoring start.
    pub elapsed_secs: f64,
    /// Millis since UNIX epoch, useful for CSV export.
    pub wall_clock_ms: i64,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub disk_bytes_per_sec: f64,
    pub net_bytes_per_sec: f64,
}

impl MetricSample {
    pub fn value_for(&self, kind: MetricKind) -> f64 {
        match kind {
            MetricKind::Cpu => self.cpu_percent as f64,
            // Display memory in MiB for readability.
            MetricKind::Memory => (self.memory_bytes as f64) / (1024.0 * 1024.0),
            // Display bandwidth in KiB/s.
            MetricKind::Disk => self.disk_bytes_per_sec / 1024.0,
            MetricKind::Network => self.net_bytes_per_sec / 1024.0,
        }
    }
}

/// Rolling history of metric samples for a single process.
///
/// History is bounded by `capacity`; the oldest samples are evicted once the
/// buffer is full so that long-running monitoring sessions do not grow
/// memory without bound.
#[derive(Debug, Clone)]
pub struct ProcessHistory {
    pub identity: ProcessIdentity,
    pub samples: VecDeque<MetricSample>,
    capacity: usize,
}

impl ProcessHistory {
    pub fn new(identity: ProcessIdentity, capacity: usize) -> Self {
        assert!(capacity > 0, "history capacity must be > 0");
        Self {
            identity,
            samples: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
        }
    }

    pub fn push(&mut self, sample: MetricSample) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns `(x, y)` points suitable for `egui_plot::Line::new`.
    pub fn series(&self, kind: MetricKind) -> Vec<[f64; 2]> {
        self.samples
            .iter()
            .map(|s| [s.elapsed_secs, s.value_for(kind)])
            .collect()
    }

    /// Returns the latest sample, if any.
    pub fn latest(&self) -> Option<&MetricSample> {
        self.samples.back()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: f64, cpu: f32, mem: u64, disk: f64, net: f64) -> MetricSample {
        MetricSample {
            elapsed_secs: t,
            wall_clock_ms: (t * 1000.0) as i64,
            cpu_percent: cpu,
            memory_bytes: mem,
            disk_bytes_per_sec: disk,
            net_bytes_per_sec: net,
        }
    }

    #[test]
    fn metric_kind_labels_are_non_empty_and_unique() {
        let labels: Vec<&str> = MetricKind::ALL.iter().map(|k| k.label()).collect();
        let shorts: Vec<&str> = MetricKind::ALL.iter().map(|k| k.short_name()).collect();
        for l in &labels {
            assert!(!l.is_empty());
        }
        let mut dedup = labels.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), labels.len(), "labels must be unique");
        let mut dedup_short = shorts.clone();
        dedup_short.sort();
        dedup_short.dedup();
        assert_eq!(
            dedup_short.len(),
            shorts.len(),
            "short names must be unique"
        );
    }

    #[test]
    fn metric_kind_all_contains_every_variant() {
        assert_eq!(MetricKind::ALL.len(), 4);
        assert!(MetricKind::ALL.contains(&MetricKind::Cpu));
        assert!(MetricKind::ALL.contains(&MetricKind::Memory));
        assert!(MetricKind::ALL.contains(&MetricKind::Disk));
        assert!(MetricKind::ALL.contains(&MetricKind::Network));
    }

    #[test]
    fn process_identity_display_label_includes_pid_and_name() {
        let id = ProcessIdentity::new(42, "target", 1_700_000_000);
        let label = id.display_label();
        assert!(label.contains("target"));
        assert!(label.contains("42"));
    }

    #[test]
    fn with_parent_sets_parent_pid() {
        let id = ProcessIdentity::new(2, "child", 0).with_parent(Some(1));
        assert_eq!(id.parent_pid, Some(1));
    }

    #[test]
    fn metric_sample_value_for_converts_units() {
        let s = sample(1.0, 12.5, 2 * 1024 * 1024, 4096.0, 8192.0);
        assert!((s.value_for(MetricKind::Cpu) - 12.5).abs() < 1e-6);
        assert!((s.value_for(MetricKind::Memory) - 2.0).abs() < 1e-6);
        assert!((s.value_for(MetricKind::Disk) - 4.0).abs() < 1e-6);
        assert!((s.value_for(MetricKind::Network) - 8.0).abs() < 1e-6);
    }

    #[test]
    fn history_bounded_by_capacity() {
        let id = ProcessIdentity::new(1, "p", 0);
        let mut h = ProcessHistory::new(id, 3);
        for i in 0..10 {
            h.push(sample(i as f64, i as f32, i as u64, 0.0, 0.0));
        }
        assert_eq!(h.len(), 3);
        // Oldest retained sample should have elapsed_secs == 7.0 (10 - 3)
        assert!((h.samples.front().unwrap().elapsed_secs - 7.0).abs() < 1e-9);
        assert!((h.latest().unwrap().elapsed_secs - 9.0).abs() < 1e-9);
    }

    #[test]
    fn history_series_returns_xy_pairs() {
        let id = ProcessIdentity::new(1, "p", 0);
        let mut h = ProcessHistory::new(id, 10);
        h.push(sample(0.0, 10.0, 1024 * 1024, 0.0, 0.0));
        h.push(sample(1.0, 20.0, 2 * 1024 * 1024, 0.0, 0.0));
        let pts = h.series(MetricKind::Cpu);
        assert_eq!(pts.len(), 2);
        assert!((pts[0][0] - 0.0).abs() < 1e-9);
        assert!((pts[0][1] - 10.0).abs() < 1e-9);
        assert!((pts[1][0] - 1.0).abs() < 1e-9);
        assert!((pts[1][1] - 20.0).abs() < 1e-9);

        let mem_pts = h.series(MetricKind::Memory);
        assert!((mem_pts[0][1] - 1.0).abs() < 1e-9);
        assert!((mem_pts[1][1] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn empty_history_has_no_latest() {
        let id = ProcessIdentity::new(1, "p", 0);
        let h = ProcessHistory::new(id, 4);
        assert!(h.is_empty());
        assert!(h.latest().is_none());
    }

    #[test]
    #[should_panic(expected = "history capacity must be > 0")]
    fn zero_capacity_history_panics() {
        let id = ProcessIdentity::new(1, "p", 0);
        let _ = ProcessHistory::new(id, 0);
    }
}
