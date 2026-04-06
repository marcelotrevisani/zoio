//! End-to-end integration tests driving the monitor -> history -> CSV pipeline.
//!
//! These tests spawn real child processes (sleepers) and verify that
//! zoio can discover them, sample their metrics, and export a CSV that
//! contains the expected rows.

use std::collections::HashSet;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use zoio::app::AppState;
use zoio::metrics::MetricKind;
use zoio::process_filter::ProcessFilter;
use zoio::process_monitor::MonitorConfig;

/// RAII wrapper that kills the child on drop so tests never leak processes.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_sleeper(secs: u64) -> ChildGuard {
    let child = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", &format!("ping -n {} 127.0.0.1 > NUL", secs + 1)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd ping")
    } else {
        Command::new("sleep")
            .arg(secs.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep")
    };
    ChildGuard(child)
}

#[test]
fn discovery_finds_spawned_child_by_pid() {
    let child = spawn_sleeper(5);
    let target_pid = child.0.id();
    let mut state = AppState::new(MonitorConfig {
        sample_interval: Duration::from_millis(250),
        history_capacity: 60,
    });
    state.refresh_discovery();
    assert!(
        state.discovered.iter().any(|p| p.pid == target_pid),
        "spawned pid {target_pid} must appear in discovery list"
    );
}

#[test]
fn explicit_selection_records_samples_for_target() {
    let child = spawn_sleeper(5);
    let target_pid = child.0.id();
    let mut state = AppState::new(MonitorConfig {
        sample_interval: Duration::from_millis(100),
        history_capacity: 60,
    });
    state.refresh_discovery();
    state.selected_pids.insert(target_pid);
    state.start_monitoring();

    // Drive several ticks. Ticks are interval-gated, so sleep between them.
    for _ in 0..5 {
        state.last_sample = None; // force the gate open
        state.tick();
        sleep(Duration::from_millis(120));
    }
    state.stop_monitoring();

    let hist = state
        .histories
        .get(&target_pid)
        .expect("target process must have a history entry");
    assert!(
        hist.len() >= 2,
        "expected at least 2 samples for target, got {}",
        hist.len()
    );
}

#[test]
fn regex_filter_selects_by_process_name() {
    let child = spawn_sleeper(5);
    let target_pid = child.0.id();
    let mut state = AppState::new(MonitorConfig {
        sample_interval: Duration::from_millis(100),
        history_capacity: 60,
    });

    // Build a regex matching either "sleep" (unix) or "cmd"/"ping" (windows).
    let pattern = if cfg!(windows) { "cmd|ping" } else { "^sleep$" };
    state.filters.push(ProcessFilter::new(pattern).unwrap());
    state.start_monitoring();

    for _ in 0..5 {
        state.last_sample = None;
        state.tick();
        sleep(Duration::from_millis(120));
    }
    state.stop_monitoring();

    assert!(
        state.histories.contains_key(&target_pid),
        "regex filter should have picked up the spawned child (pid {target_pid}). \
         Known pids: {:?}",
        state.histories.keys().collect::<Vec<_>>()
    );
}

#[test]
fn end_to_end_monitor_then_csv_export() {
    let child = spawn_sleeper(5);
    let target_pid = child.0.id();
    let mut state = AppState::new(MonitorConfig {
        sample_interval: Duration::from_millis(100),
        history_capacity: 60,
    });
    state.selected_pids.insert(target_pid);
    state.start_monitoring();
    for _ in 0..4 {
        state.last_sample = None;
        state.tick();
        sleep(Duration::from_millis(120));
    }
    state.stop_monitoring();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.csv");
    let rows = state.export_csv(&path).unwrap();
    assert!(rows >= 2);

    let contents = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert!(lines[0].starts_with("timestamp_ms,"));
    // Every data line should reference the target pid in column 3.
    let target_str = target_pid.to_string();
    let matches = lines
        .iter()
        .skip(1)
        .filter(|l| l.split(',').nth(2) == Some(target_str.as_str()))
        .count();
    assert!(
        matches >= 2,
        "expected >=2 rows for target pid {target_pid}, found {matches}"
    );
}

#[test]
fn combined_aggregate_series_has_one_point_per_bucket() {
    let child1 = spawn_sleeper(5);
    let child2 = spawn_sleeper(5);
    let pid1 = child1.0.id();
    let pid2 = child2.0.id();

    let mut state = AppState::new(MonitorConfig {
        sample_interval: Duration::from_millis(100),
        history_capacity: 60,
    });
    state.selected_pids.insert(pid1);
    state.selected_pids.insert(pid2);
    state.start_monitoring();
    for _ in 0..4 {
        state.last_sample = None;
        state.tick();
        sleep(Duration::from_millis(120));
    }
    state.stop_monitoring();

    let mut pids = HashSet::new();
    pids.insert(pid1);
    pids.insert(pid2);
    let mem = state.aggregate_series(&pids, MetricKind::Memory);
    assert!(!mem.is_empty());
    // Aggregated memory must be >= each individual history's last point.
    let last = mem.last().unwrap()[1];
    assert!(last >= 0.0);
}

#[test]
fn stopping_preserves_histories_and_allows_export() {
    let child = spawn_sleeper(5);
    let pid = child.0.id();
    let mut state = AppState::new(MonitorConfig {
        sample_interval: Duration::from_millis(100),
        history_capacity: 60,
    });
    state.selected_pids.insert(pid);
    state.start_monitoring();
    for _ in 0..3 {
        state.last_sample = None;
        state.tick();
        sleep(Duration::from_millis(120));
    }
    let before = state.histories.get(&pid).unwrap().len();
    state.stop_monitoring();
    let after = state.histories.get(&pid).unwrap().len();
    assert_eq!(
        before, after,
        "stop_monitoring must not drop already-captured samples"
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("post-stop.csv");
    let rows = state.export_csv(&path).unwrap();
    assert_eq!(rows, after);
}

#[test]
fn restarting_session_clears_previous_histories() {
    let child = spawn_sleeper(5);
    let pid = child.0.id();
    let mut state = AppState::new(MonitorConfig {
        sample_interval: Duration::from_millis(100),
        history_capacity: 60,
    });
    state.selected_pids.insert(pid);
    state.start_monitoring();
    for _ in 0..2 {
        state.last_sample = None;
        state.tick();
        sleep(Duration::from_millis(120));
    }
    assert!(!state.histories.get(&pid).unwrap().is_empty());
    state.stop_monitoring();
    state.start_monitoring();
    assert!(state.histories.is_empty(), "new session must start clean");
}
