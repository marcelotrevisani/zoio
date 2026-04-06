//! Cross-platform process monitoring backed by the `sysinfo` crate.
//!
//! The monitor exposes two responsibilities:
//!
//! 1. **Discovery** — enumerate currently-running processes so the UI can
//!    present them as checkboxes.
//! 2. **Sampling** — on each refresh tick, produce a [`SampledProcess`] for
//!    every process that matches the active [`FilterSet`], including
//!    transitive children when requested.
//!
//! The monitor keeps the previous sample of each process so that it can
//! compute instantaneous *rates* (bytes/sec) for disk and network I/O,
//! which `sysinfo` only exposes as cumulative counters.

use crate::metrics::{MetricSample, ProcessIdentity};
use crate::process_filter::FilterSet;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// Snapshot of a single process produced during a sampling pass.
#[derive(Debug, Clone)]
pub struct SampledProcess {
    pub identity: ProcessIdentity,
    pub sample: MetricSample,
    pub cmdline: String,
}

/// Configuration for a [`ProcessMonitor`].
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// Minimum interval between samples.
    pub sample_interval: Duration,
    /// Maximum number of samples retained per process in the UI-side history.
    /// (The monitor itself does not store history; this is surfaced for the app.)
    pub history_capacity: usize,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            sample_interval: Duration::from_millis(1000),
            history_capacity: 3600,
        }
    }
}

/// Previous cumulative I/O counters, used to compute per-sample deltas.
#[derive(Debug, Clone, Copy, Default)]
struct IoCounters {
    disk_read_bytes: u64,
    disk_written_bytes: u64,
    at: Option<Instant>,
}

/// The main process monitor.
///
/// The monitor is not thread-safe; it's meant to be driven from the UI
/// thread (or a dedicated sampling thread) via repeated calls to
/// [`ProcessMonitor::sample`].
pub struct ProcessMonitor {
    system: System,
    config: MonitorConfig,
    started_at: Option<Instant>,
    previous_io: HashMap<Pid, IoCounters>,
    // Cached "is running" flag.
    running: bool,
}

impl ProcessMonitor {
    pub fn new(config: MonitorConfig) -> Self {
        let refresh = RefreshKind::new().with_processes(ProcessRefreshKind::everything());
        Self {
            system: System::new_with_specifics(refresh),
            config,
            started_at: None,
            previous_io: HashMap::new(),
            running: false,
        }
    }

    pub fn config(&self) -> &MonitorConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: MonitorConfig) {
        self.config = config;
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Mark the monitor as started, resetting the time origin.
    pub fn start(&mut self) {
        self.started_at = Some(Instant::now());
        self.previous_io.clear();
        self.running = true;
    }

    /// Mark the monitor as stopped. History is preserved by the caller.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Refresh and return the list of every running process, suitable for
    /// the discovery/checkbox UI. The list is sorted by process name for
    /// stable presentation.
    pub fn discover(&mut self) -> Vec<ProcessIdentity> {
        self.system.refresh_processes(ProcessesToUpdate::All, true);
        let mut out: Vec<ProcessIdentity> = self
            .system
            .processes()
            .iter()
            .map(|(pid, proc_)| ProcessIdentity {
                pid: pid.as_u32(),
                name: proc_.name().to_string_lossy().to_string(),
                start_time: proc_.start_time(),
                parent_pid: proc_.parent().map(|p| p.as_u32()),
            })
            .collect();
        out.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        out
    }

    /// Take a sampling pass.
    ///
    /// `filters` selects which processes to sample.
    /// `explicit_pids` adds processes selected manually via the checkbox UI.
    ///
    /// When a filter requests child inclusion, or `include_children` is
    /// true, the transitive descendant set of every matched process is also
    /// included.
    pub fn sample(
        &mut self,
        filters: &FilterSet,
        explicit_pids: &HashSet<u32>,
        include_children_for_explicit: bool,
    ) -> Vec<SampledProcess> {
        self.system.refresh_processes(ProcessesToUpdate::All, true);

        let now = Instant::now();
        let elapsed = match self.started_at {
            Some(t0) => now.duration_since(t0).as_secs_f64(),
            None => 0.0,
        };
        let wall_ms = chrono::Utc::now().timestamp_millis();

        // ---- Step 1: determine selected PIDs -------------------------------
        let mut selected: HashSet<u32> = HashSet::new();
        let processes = self.system.processes();

        // Pre-compute parent -> children adjacency for child expansion.
        let mut children_of: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, proc_) in processes {
            if let Some(parent) = proc_.parent() {
                children_of
                    .entry(parent.as_u32())
                    .or_default()
                    .push(pid.as_u32());
            }
        }

        // Seed from explicit PIDs.
        for &pid in explicit_pids {
            if processes.contains_key(&Pid::from_u32(pid)) {
                selected.insert(pid);
            }
        }

        // Seed from filters.
        let filter_wants_children = filters.wants_children();
        for (pid, proc_) in processes {
            if filters.filters.is_empty() {
                break;
            }
            let name = proc_.name().to_string_lossy();
            let cmd_strings: Vec<String> = proc_
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();
            let cmdline = cmd_strings.join(" ");
            if filters.matches(&name, &cmdline) {
                selected.insert(pid.as_u32());
            }
        }

        // Expand to children if requested.
        if filter_wants_children || include_children_for_explicit {
            let mut stack: Vec<u32> = selected.iter().copied().collect();
            while let Some(parent) = stack.pop() {
                if let Some(kids) = children_of.get(&parent) {
                    for &kid in kids {
                        if selected.insert(kid) {
                            stack.push(kid);
                        }
                    }
                }
            }
        }

        // ---- Step 2: sample each selected process -------------------------
        let mut out = Vec::with_capacity(selected.len());
        for pid_u32 in &selected {
            let pid = Pid::from_u32(*pid_u32);
            let Some(proc_) = processes.get(&pid) else {
                continue;
            };

            let name = proc_.name().to_string_lossy().to_string();
            let cmd_strings: Vec<String> = proc_
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();
            let cmdline = cmd_strings.join(" ");

            // Disk I/O rate
            let io = proc_.disk_usage();
            let prev = self.previous_io.get(&pid).copied().unwrap_or_default();
            let (disk_rate, _net_rate) = if let Some(prev_at) = prev.at {
                let dt = now.duration_since(prev_at).as_secs_f64().max(1e-6);
                let d_read = io.read_bytes.saturating_sub(prev.disk_read_bytes);
                let d_write = io.written_bytes.saturating_sub(prev.disk_written_bytes);
                (((d_read + d_write) as f64) / dt, 0.0)
            } else {
                (0.0, 0.0)
            };
            self.previous_io.insert(
                pid,
                IoCounters {
                    disk_read_bytes: io.read_bytes,
                    disk_written_bytes: io.written_bytes,
                    at: Some(now),
                },
            );

            // `sysinfo` does not expose per-process network counters on all
            // platforms. We report zero rather than fabricate numbers; when a
            // platform does support it, swap in real deltas here.
            let net_rate = 0.0;

            let sample = MetricSample {
                elapsed_secs: elapsed,
                wall_clock_ms: wall_ms,
                cpu_percent: proc_.cpu_usage(),
                memory_bytes: proc_.memory(),
                disk_bytes_per_sec: disk_rate,
                net_bytes_per_sec: net_rate,
            };

            let identity = ProcessIdentity {
                pid: *pid_u32,
                name: name.clone(),
                start_time: proc_.start_time(),
                parent_pid: proc_.parent().map(|p| p.as_u32()),
            };

            out.push(SampledProcess {
                identity,
                sample,
                cmdline,
            });
        }

        // Garbage-collect io counters for processes that are no longer
        // running (or no longer selected). This prevents unbounded growth.
        self.previous_io
            .retain(|pid, _| processes.contains_key(pid) && selected.contains(&pid.as_u32()));

        out.sort_by(|a, b| a.identity.pid.cmp(&b.identity.pid));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_filter::{FilterSet, ProcessFilter};

    #[test]
    fn monitor_config_defaults_are_sensible() {
        let c = MonitorConfig::default();
        assert!(c.sample_interval >= Duration::from_millis(100));
        assert!(c.history_capacity >= 60);
    }

    #[test]
    fn start_and_stop_toggle_running_flag() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        assert!(!m.is_running());
        m.start();
        assert!(m.is_running());
        m.stop();
        assert!(!m.is_running());
    }

    #[test]
    fn discover_returns_at_least_the_current_process() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        let ids = m.discover();
        assert!(!ids.is_empty(), "process list should not be empty");
        let self_pid = std::process::id();
        assert!(
            ids.iter().any(|i| i.pid == self_pid),
            "discovery should include the test process itself (pid {self_pid})"
        );
    }

    #[test]
    fn discover_results_sorted_by_name() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        let ids = m.discover();
        let names: Vec<String> = ids.iter().map(|i| i.name.to_ascii_lowercase()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "discover() must return name-sorted list");
    }

    #[test]
    fn sample_returns_only_explicitly_selected_pid_with_no_filters() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        m.start();
        let self_pid = std::process::id();
        let mut explicit = HashSet::new();
        explicit.insert(self_pid);
        let filters = FilterSet::new();
        let result = m.sample(&filters, &explicit, false);
        assert!(!result.is_empty(), "sample should return at least self");
        assert!(result.iter().any(|s| s.identity.pid == self_pid));
    }

    #[test]
    fn sample_uses_filters_to_select_processes() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        m.start();
        // First, discover a real process name to build a matching filter.
        let ids = m.discover();
        let self_pid = std::process::id();
        let self_name = ids
            .iter()
            .find(|i| i.pid == self_pid)
            .map(|i| i.name.clone())
            .expect("self must be discoverable");

        // Build a regex that matches the self process by an anchored name.
        let mut fs = FilterSet::new();
        let escaped = regex::escape(&self_name);
        let mut filt = ProcessFilter::new(format!("^{escaped}$")).unwrap();
        filt.include_children = false;
        fs.push(filt);

        let result = m.sample(&fs, &HashSet::new(), false);
        assert!(
            result.iter().any(|s| s.identity.pid == self_pid),
            "filtering by self process name must include self; got {} results",
            result.len()
        );
    }

    #[test]
    fn disk_rate_is_zero_on_first_sample() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        m.start();
        let mut explicit = HashSet::new();
        explicit.insert(std::process::id());
        let r = m.sample(&FilterSet::new(), &explicit, false);
        let s = r
            .iter()
            .find(|p| p.identity.pid == std::process::id())
            .unwrap();
        assert_eq!(
            s.sample.disk_bytes_per_sec, 0.0,
            "first sample cannot compute a rate"
        );
    }

    #[test]
    fn sampling_without_start_still_works_with_zero_elapsed() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        // Not started — elapsed should be 0.
        let mut explicit = HashSet::new();
        explicit.insert(std::process::id());
        let r = m.sample(&FilterSet::new(), &explicit, false);
        let s = r
            .iter()
            .find(|p| p.identity.pid == std::process::id())
            .unwrap();
        assert_eq!(s.sample.elapsed_secs, 0.0);
    }

    #[test]
    fn sample_ignores_nonexistent_explicit_pids() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        m.start();
        let mut explicit = HashSet::new();
        // A PID that almost certainly does not exist.
        explicit.insert(u32::MAX - 1);
        let r = m.sample(&FilterSet::new(), &explicit, false);
        assert!(r.is_empty(), "nonexistent pid must be filtered out");
    }

    #[test]
    fn set_config_replaces_config() {
        let mut m = ProcessMonitor::new(MonitorConfig::default());
        let new_cfg = MonitorConfig {
            sample_interval: Duration::from_millis(500),
            history_capacity: 120,
        };
        m.set_config(new_cfg.clone());
        assert_eq!(m.config().sample_interval, new_cfg.sample_interval);
        assert_eq!(m.config().history_capacity, new_cfg.history_capacity);
    }
}
