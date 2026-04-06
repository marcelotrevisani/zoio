//! The egui-based application that drives the real-time UI.
//!
//! The app keeps these pieces of state:
//!
//! * `discovered` — the latest enumeration of running processes, shown as
//!   a searchable checkbox list.
//! * `selected_pids` — the set of pids the user has explicitly ticked.
//! * `filters` — user-authored regex filters.
//! * `histories` — per-process rolling metric histories that feed the plots.
//! * `groups` — named bundles of pids so the user can plot a group as one
//!   aggregated line.
//!
//! The app is structured so that the data pipeline (monitor -> histories
//! -> plots -> CSV) can be driven from tests without needing a real
//! display.

use crate::csv_export::export_to_csv;
use crate::metrics::{MetricKind, ProcessHistory, ProcessIdentity};
use crate::process_filter::{FilterSet, ProcessFilter};
use crate::process_monitor::{MonitorConfig, ProcessMonitor};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A named bundle of PIDs whose metrics should be plotted as a single
/// aggregated series.
#[derive(Debug, Clone, Default)]
pub struct ProcessGroup {
    pub name: String,
    pub members: HashSet<u32>,
    pub visible: bool,
}

/// What the plots should display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlotView {
    /// One line per selected process.
    #[default]
    Individual,
    /// One aggregated line combining every selected process.
    Combined,
    /// One aggregated line per visible group.
    Grouped,
}

/// Shared state for the zoio application.
///
/// This struct does not depend on egui so it can be unit-tested headlessly.
pub struct AppState {
    pub monitor: ProcessMonitor,
    pub discovered: Vec<ProcessIdentity>,
    pub selected_pids: HashSet<u32>,
    pub filters: FilterSet,
    pub histories: HashMap<u32, ProcessHistory>,
    pub groups: Vec<ProcessGroup>,
    pub plot_view: PlotView,
    pub search_query: String,
    pub last_sample: Option<Instant>,
    pub last_discover: Option<Instant>,
    pub status: String,
    pub include_children_for_selection: bool,
    pub pending_filter_pattern: String,
    pub pending_group_name: String,
    pub tree_view: bool,
}

impl AppState {
    pub fn new(config: MonitorConfig) -> Self {
        Self {
            monitor: ProcessMonitor::new(config),
            discovered: Vec::new(),
            selected_pids: HashSet::new(),
            filters: FilterSet::new(),
            histories: HashMap::new(),
            groups: Vec::new(),
            plot_view: PlotView::default(),
            search_query: String::new(),
            last_sample: None,
            last_discover: None,
            status: "idle".to_string(),
            include_children_for_selection: true,
            pending_filter_pattern: String::new(),
            pending_group_name: String::new(),
            tree_view: false,
        }
    }

    /// Refresh the discovered process list.
    pub fn refresh_discovery(&mut self) {
        self.discovered = self.monitor.discover();
        self.last_discover = Some(Instant::now());
    }

    /// Begin a new monitoring session. Clears all prior histories so the
    /// next CSV export only contains samples from *this* session.
    pub fn start_monitoring(&mut self) {
        self.histories.clear();
        self.monitor.start();
        self.last_sample = None;
        self.status = "monitoring".to_string();
    }

    /// Stop the current monitoring session. Histories are retained so the
    /// user can still inspect plots and export to CSV.
    pub fn stop_monitoring(&mut self) {
        self.monitor.stop();
        self.status = "stopped".to_string();
    }

    pub fn is_monitoring(&self) -> bool {
        self.monitor.is_running()
    }

    /// Drive one sampling tick. Returns the number of process samples
    /// recorded. No-op if monitoring is not running or the configured
    /// interval has not elapsed.
    pub fn tick(&mut self) -> usize {
        if !self.monitor.is_running() {
            return 0;
        }
        let interval = self.monitor.config().sample_interval;
        if let Some(t) = self.last_sample {
            if t.elapsed() < interval {
                return 0;
            }
        }
        self.last_sample = Some(Instant::now());

        let capacity = self.monitor.config().history_capacity;
        let samples = self.monitor.sample(
            &self.filters,
            &self.selected_pids,
            self.include_children_for_selection,
        );
        let n = samples.len();
        for sp in samples {
            let id = sp.identity.clone();
            let entry = self
                .histories
                .entry(id.pid)
                .or_insert_with(|| ProcessHistory::new(id, capacity));
            entry.push(sp.sample);
        }
        n
    }

    /// Add a new user-authored filter. Returns an error string for display
    /// if the pattern is invalid.
    pub fn add_filter(&mut self, pattern: &str) -> Result<(), String> {
        if pattern.trim().is_empty() {
            return Err("pattern must not be empty".to_string());
        }
        match ProcessFilter::new(pattern) {
            Ok(f) => {
                self.filters.push(f);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Export the current histories to a CSV file at `path`.
    pub fn export_csv(&self, path: impl Into<PathBuf>) -> anyhow::Result<usize> {
        let path = path.into();
        let count = export_to_csv(&path, self.histories.values())?;
        Ok(count)
    }

    /// Create a new group from the currently-selected PIDs.
    pub fn create_group(&mut self, name: impl Into<String>) -> Result<(), String> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("group name must not be empty".to_string());
        }
        if self.groups.iter().any(|g| g.name == name) {
            return Err(format!("group '{name}' already exists"));
        }
        self.groups.push(ProcessGroup {
            name,
            members: self.selected_pids.clone(),
            visible: true,
        });
        Ok(())
    }

    pub fn remove_group(&mut self, name: &str) {
        self.groups.retain(|g| g.name != name);
    }

    /// Compute an aggregated series for a given metric across a set of PIDs.
    /// Samples are bucketed by their `elapsed_secs` key (rounded to the
    /// nearest sampling interval) and summed.
    pub fn aggregate_series(&self, pids: &HashSet<u32>, kind: MetricKind) -> Vec<[f64; 2]> {
        let mut buckets: BTreeMap<i64, f64> = BTreeMap::new();
        let interval_ms = self.monitor.config().sample_interval.as_millis().max(1) as i64;
        for pid in pids {
            if let Some(h) = self.histories.get(pid) {
                for s in &h.samples {
                    let key = (s.elapsed_secs * 1000.0) as i64 / interval_ms;
                    *buckets.entry(key).or_insert(0.0) += s.value_for(kind);
                }
            }
        }
        buckets
            .into_iter()
            .map(|(key, v)| [(key as f64) * (interval_ms as f64) / 1000.0, v])
            .collect()
    }

    /// Return the pid-indexed filtered discovery list honoring the current
    /// search query. Case-insensitive substring match against name or pid.
    pub fn filtered_discovery(&self) -> Vec<&ProcessIdentity> {
        let q = self.search_query.trim().to_ascii_lowercase();
        self.discovered
            .iter()
            .filter(|id| {
                if q.is_empty() {
                    return true;
                }
                id.name.to_ascii_lowercase().contains(&q) || id.pid.to_string().contains(&q)
            })
            .collect()
    }

    /// Build a flattened tree representation of the discovered processes.
    ///
    /// Each entry is `(depth, &ProcessIdentity)`. Roots are processes whose
    /// parent is not present in the discovery list (or has no parent).
    /// Children are sorted alphabetically under their parent, matching the
    /// flat list order. When a search query is active, the tree is filtered
    /// to only show matching processes (and the tree degenerates to depth 0
    /// since intermediate ancestors may not match).
    pub fn build_process_tree(&self) -> Vec<(usize, &ProcessIdentity)> {
        let q = self.search_query.trim().to_ascii_lowercase();
        let has_query = !q.is_empty();

        // If there is a search query, fall back to a flat filtered list
        // because showing partial trees with missing ancestors is confusing.
        if has_query {
            return self
                .filtered_discovery()
                .into_iter()
                .map(|id| (0, id))
                .collect();
        }

        let pid_set: HashSet<u32> = self.discovered.iter().map(|p| p.pid).collect();

        // parent_pid -> sorted children
        let mut children_of: HashMap<u32, Vec<&ProcessIdentity>> = HashMap::new();
        let mut roots: Vec<&ProcessIdentity> = Vec::new();

        for id in &self.discovered {
            match id.parent_pid {
                Some(ppid) if pid_set.contains(&ppid) => {
                    children_of.entry(ppid).or_default().push(id);
                }
                _ => roots.push(id),
            }
        }

        // Sort children alphabetically by name (discovered is already sorted,
        // but children within a parent may not be contiguous).
        for kids in children_of.values_mut() {
            kids.sort_by(|a, b| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            });
        }

        let mut result = Vec::with_capacity(self.discovered.len());
        let mut stack: Vec<(usize, &ProcessIdentity)> = Vec::new();

        // Push roots in reverse so the first root is processed first.
        for r in roots.iter().rev() {
            stack.push((0, r));
        }

        while let Some((depth, node)) = stack.pop() {
            result.push((depth, node));
            if let Some(kids) = children_of.get(&node.pid) {
                for kid in kids.iter().rev() {
                    stack.push((depth + 1, kid));
                }
            }
        }

        result
    }
}

// ---------------------------------------------------------------------------
// egui rendering layer
// ---------------------------------------------------------------------------

/// The eframe application that wraps [`AppState`] and renders the UI.
pub struct ZoioApp {
    pub state: AppState,
    /// Auto-refresh interval for the discovery list.
    pub discovery_interval: Duration,
    /// Last filter-error message to display in a popup-style label.
    pub last_error: Option<String>,
}

impl ZoioApp {
    pub fn new(config: MonitorConfig) -> Self {
        let mut state = AppState::new(config);
        state.refresh_discovery();
        Self {
            state,
            discovery_interval: Duration::from_secs(3),
            last_error: None,
        }
    }
}

impl eframe::App for ZoioApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        use eframe::egui;

        // Refresh discovery list on an interval.
        if self
            .state
            .last_discover
            .map(|t| t.elapsed() >= self.discovery_interval)
            .unwrap_or(true)
        {
            self.state.refresh_discovery();
        }

        // Drive one sampling tick; ask egui to repaint soon.
        self.state.tick();
        ctx.request_repaint_after(self.state.monitor.config().sample_interval);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("zoio");
                ui.label(
                    egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .small()
                        .weak(),
                );
                ui.separator();
                let running = self.state.is_monitoring();
                if ui
                    .add_enabled(!running, egui::Button::new("▶ Start"))
                    .clicked()
                {
                    self.state.start_monitoring();
                }
                if ui
                    .add_enabled(running, egui::Button::new("⏹ Stop"))
                    .clicked()
                {
                    self.state.stop_monitoring();
                }
                if ui.button("💾 Export CSV…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("CSV", &["csv"])
                        .set_file_name("zoio-export.csv")
                        .save_file()
                    {
                        match self.state.export_csv(&path) {
                            Ok(n) => {
                                self.state.status = format!("exported {n} rows");
                                self.last_error = None;
                            }
                            Err(e) => {
                                self.last_error = Some(format!("export failed: {e}"));
                            }
                        }
                    }
                }
                ui.separator();
                ui.label(format!("status: {}", self.state.status));
                ui.label(format!("tracked: {} processes", self.state.histories.len()));
            });
            if let Some(err) = &self.last_error {
                ui.colored_label(egui::Color32::RED, err);
            }
        });

        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.heading("Filters");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.state.pending_filter_pattern);
                    if ui.button("+ Add").clicked() {
                        let p = self.state.pending_filter_pattern.clone();
                        match self.state.add_filter(&p) {
                            Ok(()) => {
                                self.state.pending_filter_pattern.clear();
                                self.last_error = None;
                            }
                            Err(e) => self.last_error = Some(e),
                        }
                    }
                });
                let mut remove_idx: Option<usize> = None;
                for (i, f) in self.state.filters.filters.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut f.enabled, "");
                        ui.monospace(f.pattern());
                        ui.checkbox(&mut f.include_children, "children");
                        if ui.small_button("✕").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_idx {
                    self.state.filters.remove(i);
                }
                ui.separator();

                ui.horizontal(|ui| {
                    ui.heading("Processes");
                    if ui.button("⟳ Refresh").clicked() {
                        self.state.refresh_discovery();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    ui.text_edit_singleline(&mut self.state.search_query);
                });
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut self.state.include_children_for_selection,
                        "include children",
                    );
                    ui.checkbox(&mut self.state.tree_view, "tree view");
                });

                // --- rows of the discovered process list ---
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        if self.state.tree_view {
                            let tree: Vec<(usize, u32, String)> = self
                                .state
                                .build_process_tree()
                                .into_iter()
                                .map(|(depth, id)| {
                                    let indent = "  ".repeat(depth);
                                    let prefix = if depth > 0 { "└ " } else { "" };
                                    let label =
                                        format!("{indent}{prefix}{} (pid {})", id.name, id.pid);
                                    (depth, id.pid, label)
                                })
                                .collect();
                            for (_depth, pid, label) in tree {
                                let mut checked = self.state.selected_pids.contains(&pid);
                                if ui.checkbox(&mut checked, label).changed() {
                                    if checked {
                                        self.state.selected_pids.insert(pid);
                                    } else {
                                        self.state.selected_pids.remove(&pid);
                                    }
                                }
                            }
                        } else {
                            let rows: Vec<(u32, String)> = self
                                .state
                                .filtered_discovery()
                                .iter()
                                .map(|id| (id.pid, id.display_label()))
                                .collect();
                            for (pid, label) in rows {
                                let mut checked = self.state.selected_pids.contains(&pid);
                                if ui.checkbox(&mut checked, label).changed() {
                                    if checked {
                                        self.state.selected_pids.insert(pid);
                                    } else {
                                        self.state.selected_pids.remove(&pid);
                                    }
                                }
                            }
                        }
                    });

                ui.separator();
                ui.heading("Groups");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.state.pending_group_name);
                    if ui.button("+ Group selected").clicked() {
                        let name = self.state.pending_group_name.clone();
                        match self.state.create_group(&name) {
                            Ok(()) => {
                                self.state.pending_group_name.clear();
                                self.last_error = None;
                            }
                            Err(e) => self.last_error = Some(e),
                        }
                    }
                });
                let mut to_remove: Option<String> = None;
                for g in self.state.groups.iter_mut() {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut g.visible, "");
                        ui.label(format!("{} ({} pids)", g.name, g.members.len()));
                        if ui.small_button("✕").clicked() {
                            to_remove = Some(g.name.clone());
                        }
                    });
                }
                if let Some(n) = to_remove {
                    self.state.remove_group(&n);
                }

                ui.separator();
                ui.heading("Plot View");
                ui.radio_value(
                    &mut self.state.plot_view,
                    PlotView::Individual,
                    "Individual",
                );
                ui.radio_value(&mut self.state.plot_view, PlotView::Combined, "Combined");
                ui.radio_value(&mut self.state.plot_view, PlotView::Grouped, "Grouped");
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            use egui_plot::{Line, Plot, PlotPoints};
            egui::ScrollArea::vertical().show(ui, |ui| {
                for kind in MetricKind::ALL {
                    ui.heading(kind.label());
                    Plot::new(format!("plot_{}", kind.short_name()))
                        .height(180.0)
                        .legend(egui_plot::Legend::default())
                        .show(ui, |plot_ui| match self.state.plot_view {
                            PlotView::Individual => {
                                for (pid, hist) in &self.state.histories {
                                    let pts: PlotPoints = PlotPoints::new(hist.series(kind));
                                    plot_ui.line(
                                        Line::new(pts)
                                            .name(format!("{} (pid {})", hist.identity.name, pid)),
                                    );
                                }
                            }
                            PlotView::Combined => {
                                let pids: HashSet<u32> =
                                    self.state.histories.keys().copied().collect();
                                let pts = PlotPoints::new(self.state.aggregate_series(&pids, kind));
                                plot_ui.line(Line::new(pts).name("ALL"));
                            }
                            PlotView::Grouped => {
                                for g in &self.state.groups {
                                    if !g.visible {
                                        continue;
                                    }
                                    let pts = PlotPoints::new(
                                        self.state.aggregate_series(&g.members, kind),
                                    );
                                    plot_ui.line(Line::new(pts).name(g.name.clone()));
                                }
                            }
                        });
                    ui.add_space(6.0);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MetricSample;

    fn push_sample(state: &mut AppState, pid: u32, name: &str, t: f64, v: f32) {
        let id = ProcessIdentity::new(pid, name, 0);
        let cap = state.monitor.config().history_capacity;
        let h = state
            .histories
            .entry(pid)
            .or_insert_with(|| ProcessHistory::new(id, cap));
        h.push(MetricSample {
            elapsed_secs: t,
            wall_clock_ms: (t * 1000.0) as i64,
            cpu_percent: v,
            memory_bytes: 1024 * 1024,
            disk_bytes_per_sec: 0.0,
            net_bytes_per_sec: 0.0,
        });
    }

    #[test]
    fn new_state_is_idle_and_empty() {
        let s = AppState::new(MonitorConfig::default());
        assert!(!s.is_monitoring());
        assert!(s.selected_pids.is_empty());
        assert!(s.histories.is_empty());
        assert!(s.groups.is_empty());
        assert_eq!(s.status, "idle");
        assert_eq!(s.plot_view, PlotView::Individual);
    }

    #[test]
    fn start_then_stop_transitions_state() {
        let mut s = AppState::new(MonitorConfig::default());
        s.start_monitoring();
        assert!(s.is_monitoring());
        assert_eq!(s.status, "monitoring");
        s.stop_monitoring();
        assert!(!s.is_monitoring());
        assert_eq!(s.status, "stopped");
    }

    #[test]
    fn start_clears_old_histories_for_a_fresh_session() {
        let mut s = AppState::new(MonitorConfig::default());
        push_sample(&mut s, 1, "a", 0.0, 10.0);
        assert_eq!(s.histories.len(), 1);
        s.start_monitoring();
        assert!(s.histories.is_empty());
    }

    #[test]
    fn stop_retains_histories_for_later_export() {
        let mut s = AppState::new(MonitorConfig::default());
        s.start_monitoring();
        push_sample(&mut s, 1, "a", 0.0, 10.0);
        s.stop_monitoring();
        assert_eq!(s.histories.len(), 1);
    }

    #[test]
    fn add_filter_rejects_empty_and_invalid() {
        let mut s = AppState::new(MonitorConfig::default());
        assert!(s.add_filter("   ").is_err());
        assert!(s.add_filter("(unclosed").is_err());
        assert!(s.filters.filters.is_empty());
        assert!(s.add_filter("python").is_ok());
        assert_eq!(s.filters.filters.len(), 1);
    }

    #[test]
    fn create_group_requires_non_empty_unique_name() {
        let mut s = AppState::new(MonitorConfig::default());
        s.selected_pids.insert(1);
        assert!(s.create_group("  ").is_err());
        assert!(s.create_group("workers").is_ok());
        assert!(s.create_group("workers").is_err(), "duplicate not allowed");
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].members.len(), 1);
    }

    #[test]
    fn remove_group_removes_matching_name_only() {
        let mut s = AppState::new(MonitorConfig::default());
        s.selected_pids.insert(1);
        s.create_group("a").unwrap();
        s.create_group("b").unwrap();
        s.remove_group("a");
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].name, "b");
    }

    #[test]
    fn aggregate_series_sums_buckets_across_pids() {
        let mut s = AppState::new(MonitorConfig {
            sample_interval: Duration::from_millis(1000),
            history_capacity: 100,
        });
        push_sample(&mut s, 1, "a", 0.0, 10.0);
        push_sample(&mut s, 1, "a", 1.0, 20.0);
        push_sample(&mut s, 2, "b", 0.0, 30.0);
        push_sample(&mut s, 2, "b", 1.0, 40.0);

        let mut pids = HashSet::new();
        pids.insert(1);
        pids.insert(2);
        let series = s.aggregate_series(&pids, MetricKind::Cpu);
        assert_eq!(series.len(), 2);
        assert!((series[0][1] - 40.0).abs() < 1e-6, "bucket 0 = 10+30");
        assert!((series[1][1] - 60.0).abs() < 1e-6, "bucket 1 = 20+40");
    }

    #[test]
    fn aggregate_series_empty_when_no_matching_pids() {
        let s = AppState::new(MonitorConfig::default());
        let mut pids = HashSet::new();
        pids.insert(9_999);
        let series = s.aggregate_series(&pids, MetricKind::Cpu);
        assert!(series.is_empty());
    }

    #[test]
    fn export_csv_writes_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        let mut s = AppState::new(MonitorConfig::default());
        push_sample(&mut s, 1, "a", 0.0, 10.0);
        push_sample(&mut s, 1, "a", 1.0, 20.0);
        let n = s.export_csv(&path).unwrap();
        assert_eq!(n, 2);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("timestamp_ms"));
    }

    #[test]
    fn tick_is_noop_when_not_running() {
        let mut s = AppState::new(MonitorConfig::default());
        assert_eq!(s.tick(), 0);
    }

    #[test]
    fn tick_is_noop_inside_interval() {
        let mut s = AppState::new(MonitorConfig {
            sample_interval: Duration::from_secs(60),
            history_capacity: 100,
        });
        s.start_monitoring();
        s.last_sample = Some(Instant::now()); // just sampled
        assert_eq!(s.tick(), 0);
    }

    #[test]
    fn filtered_discovery_matches_name_and_pid_substring() {
        let mut s = AppState::new(MonitorConfig::default());
        s.discovered = vec![
            ProcessIdentity::new(1, "alpha", 0),
            ProcessIdentity::new(22, "beta", 0),
            ProcessIdentity::new(333, "gamma", 0),
        ];
        s.search_query = "bet".to_string();
        let r = s.filtered_discovery();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].pid, 22);

        s.search_query = "33".to_string();
        let r = s.filtered_discovery();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].pid, 333);

        s.search_query = "".to_string();
        let r = s.filtered_discovery();
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn filtered_discovery_is_case_insensitive() {
        let mut s = AppState::new(MonitorConfig::default());
        s.discovered = vec![ProcessIdentity::new(1, "MyApp", 0)];
        s.search_query = "myapp".to_string();
        assert_eq!(s.filtered_discovery().len(), 1);
        s.search_query = "MYAPP".to_string();
        assert_eq!(s.filtered_discovery().len(), 1);
    }

    #[test]
    fn refresh_discovery_populates_list_and_updates_timestamp() {
        let mut s = AppState::new(MonitorConfig::default());
        assert!(s.discovered.is_empty());
        assert!(s.last_discover.is_none());
        s.refresh_discovery();
        assert!(
            !s.discovered.is_empty(),
            "discovery should find at least one process"
        );
        assert!(s.last_discover.is_some());
    }

    #[test]
    fn refresh_discovery_updates_list_with_current_processes() {
        let mut s = AppState::new(MonitorConfig::default());
        s.refresh_discovery();
        let self_pid = std::process::id();
        assert!(
            s.discovered.iter().any(|p| p.pid == self_pid),
            "refresh should include the test process itself"
        );
    }

    #[test]
    fn repeated_refresh_updates_timestamp() {
        let mut s = AppState::new(MonitorConfig::default());
        s.refresh_discovery();
        let first = s.last_discover.unwrap();
        std::thread::sleep(Duration::from_millis(10));
        s.refresh_discovery();
        let second = s.last_discover.unwrap();
        assert!(
            second > first,
            "second refresh should have a later timestamp"
        );
    }

    // --- tree view tests ---

    fn make_tree_state() -> AppState {
        let mut s = AppState::new(MonitorConfig::default());
        // Build a small tree:
        //   init (pid 1)
        //   ├── bash (pid 10, parent 1)
        //   │   └── python (pid 100, parent 10)
        //   └── cron (pid 20, parent 1)
        //   systemd (pid 2) — root with no children
        s.discovered = vec![
            ProcessIdentity::new(1, "init", 0),
            ProcessIdentity::new(10, "bash", 0).with_parent(Some(1)),
            ProcessIdentity::new(100, "python", 0).with_parent(Some(10)),
            ProcessIdentity::new(20, "cron", 0).with_parent(Some(1)),
            ProcessIdentity::new(2, "systemd", 0),
        ];
        s
    }

    #[test]
    fn tree_view_defaults_to_off() {
        let s = AppState::new(MonitorConfig::default());
        assert!(!s.tree_view);
    }

    #[test]
    fn build_process_tree_roots_have_depth_zero() {
        let s = make_tree_state();
        let tree = s.build_process_tree();
        let roots: Vec<_> = tree.iter().filter(|(d, _)| *d == 0).collect();
        let root_pids: HashSet<u32> = roots.iter().map(|(_, id)| id.pid).collect();
        assert!(root_pids.contains(&1), "init should be a root");
        assert!(root_pids.contains(&2), "systemd should be a root");
        assert!(!root_pids.contains(&10), "bash is a child, not a root");
    }

    #[test]
    fn build_process_tree_children_have_correct_depth() {
        let s = make_tree_state();
        let tree = s.build_process_tree();
        let bash = tree.iter().find(|(_, id)| id.pid == 10).unwrap();
        assert_eq!(bash.0, 1, "bash should be at depth 1");
        let python = tree.iter().find(|(_, id)| id.pid == 100).unwrap();
        assert_eq!(python.0, 2, "python should be at depth 2");
        let cron = tree.iter().find(|(_, id)| id.pid == 20).unwrap();
        assert_eq!(cron.0, 1, "cron should be at depth 1");
    }

    #[test]
    fn build_process_tree_contains_all_discovered() {
        let s = make_tree_state();
        let tree = s.build_process_tree();
        assert_eq!(tree.len(), s.discovered.len());
        let tree_pids: HashSet<u32> = tree.iter().map(|(_, id)| id.pid).collect();
        for d in &s.discovered {
            assert!(tree_pids.contains(&d.pid));
        }
    }

    #[test]
    fn build_process_tree_parent_appears_before_children() {
        let s = make_tree_state();
        let tree = s.build_process_tree();
        let positions: HashMap<u32, usize> = tree
            .iter()
            .enumerate()
            .map(|(i, (_, id))| (id.pid, i))
            .collect();
        // init (1) before bash (10) before python (100)
        assert!(positions[&1] < positions[&10]);
        assert!(positions[&10] < positions[&100]);
        // init (1) before cron (20)
        assert!(positions[&1] < positions[&20]);
    }

    #[test]
    fn build_process_tree_with_search_falls_back_to_flat() {
        let mut s = make_tree_state();
        s.search_query = "python".to_string();
        let tree = s.build_process_tree();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].0, 0, "filtered results should be at depth 0");
        assert_eq!(tree[0].1.pid, 100);
    }

    #[test]
    fn build_process_tree_empty_discovery_returns_empty() {
        let s = AppState::new(MonitorConfig::default());
        let tree = s.build_process_tree();
        assert!(tree.is_empty());
    }

    #[test]
    fn build_process_tree_orphan_parent_becomes_root() {
        let mut s = AppState::new(MonitorConfig::default());
        // Parent pid 999 is not in the discovery list.
        s.discovered = vec![ProcessIdentity::new(5, "orphan", 0).with_parent(Some(999))];
        let tree = s.build_process_tree();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].0, 0, "orphan with missing parent should be a root");
    }

    #[test]
    fn build_process_tree_children_sorted_alphabetically() {
        let mut s = AppState::new(MonitorConfig::default());
        s.discovered = vec![
            ProcessIdentity::new(1, "parent", 0),
            ProcessIdentity::new(3, "zebra", 0).with_parent(Some(1)),
            ProcessIdentity::new(2, "alpha", 0).with_parent(Some(1)),
        ];
        let tree = s.build_process_tree();
        let children: Vec<&str> = tree
            .iter()
            .filter(|(d, _)| *d == 1)
            .map(|(_, id)| id.name.as_str())
            .collect();
        assert_eq!(children, vec!["alpha", "zebra"]);
    }
}
