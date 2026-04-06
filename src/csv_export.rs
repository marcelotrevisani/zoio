//! Export captured monitoring data to a CSV file.
//!
//! The emitted schema is:
//!
//! ```csv
//! timestamp_ms,elapsed_secs,pid,name,parent_pid,cpu_percent,memory_bytes,disk_bytes_per_sec,net_bytes_per_sec
//! ```
//!
//! One row per (process, sample). Rows are sorted by `(timestamp_ms, pid)`
//! so that downstream tools (pandas, Excel, R) can diff rows directly.

use crate::metrics::ProcessHistory;
use std::io::Write;
use std::path::Path;

pub const CSV_HEADERS: &[&str] = &[
    "timestamp_ms",
    "elapsed_secs",
    "pid",
    "name",
    "parent_pid",
    "cpu_percent",
    "memory_bytes",
    "disk_bytes_per_sec",
    "net_bytes_per_sec",
];

/// Export a collection of process histories to a CSV file at `path`.
///
/// The `histories` iterator may be any iterable of [`ProcessHistory`]
/// references. Returns the total number of data rows written (header row
/// not counted).
pub fn export_to_csv<'a, I, P>(path: P, histories: I) -> Result<usize, csv::Error>
where
    I: IntoIterator<Item = &'a ProcessHistory>,
    P: AsRef<Path>,
{
    let file = std::fs::File::create(path)?;
    write_csv(file, histories)
}

/// Write CSV output to any `Write` sink. Useful for tests and pipes.
pub fn write_csv<'a, W, I>(writer: W, histories: I) -> Result<usize, csv::Error>
where
    W: Write,
    I: IntoIterator<Item = &'a ProcessHistory>,
{
    let mut wtr = csv::Writer::from_writer(writer);
    wtr.write_record(CSV_HEADERS)?;

    // Flatten then sort for deterministic output.
    let mut rows: Vec<Row> = Vec::new();
    for hist in histories {
        for s in &hist.samples {
            rows.push(Row {
                timestamp_ms: s.wall_clock_ms,
                elapsed_secs: s.elapsed_secs,
                pid: hist.identity.pid,
                name: hist.identity.name.clone(),
                parent_pid: hist.identity.parent_pid,
                cpu_percent: s.cpu_percent,
                memory_bytes: s.memory_bytes,
                disk_bytes_per_sec: s.disk_bytes_per_sec,
                net_bytes_per_sec: s.net_bytes_per_sec,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.timestamp_ms
            .cmp(&b.timestamp_ms)
            .then_with(|| a.pid.cmp(&b.pid))
    });

    let count = rows.len();
    for r in rows {
        wtr.write_record(&[
            r.timestamp_ms.to_string(),
            format!("{:.6}", r.elapsed_secs),
            r.pid.to_string(),
            r.name,
            r.parent_pid.map(|p| p.to_string()).unwrap_or_default(),
            format!("{:.4}", r.cpu_percent),
            r.memory_bytes.to_string(),
            format!("{:.2}", r.disk_bytes_per_sec),
            format!("{:.2}", r.net_bytes_per_sec),
        ])?;
    }
    wtr.flush()?;
    Ok(count)
}

struct Row {
    timestamp_ms: i64,
    elapsed_secs: f64,
    pid: u32,
    name: String,
    parent_pid: Option<u32>,
    cpu_percent: f32,
    memory_bytes: u64,
    disk_bytes_per_sec: f64,
    net_bytes_per_sec: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{MetricSample, ProcessHistory, ProcessIdentity};

    fn fixture(pid: u32, name: &str, parent: Option<u32>, samples: usize) -> ProcessHistory {
        let id = ProcessIdentity::new(pid, name, 1_700_000_000).with_parent(parent);
        let mut h = ProcessHistory::new(id, 1024);
        for i in 0..samples {
            h.push(MetricSample {
                elapsed_secs: i as f64,
                wall_clock_ms: 1_700_000_000_000 + (i as i64) * 1000,
                cpu_percent: 10.0 + i as f32,
                memory_bytes: 1024 * 1024 * (i as u64 + 1),
                disk_bytes_per_sec: 100.0 * i as f64,
                net_bytes_per_sec: 50.0 * i as f64,
            });
        }
        h
    }

    #[test]
    fn csv_has_correct_header_and_row_count() {
        let h1 = fixture(1, "proc1", None, 3);
        let h2 = fixture(2, "proc2", Some(1), 2);
        let mut buf = Vec::new();
        let n = write_csv(&mut buf, [&h1, &h2]).unwrap();
        assert_eq!(n, 5);
        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        // header + 5 rows
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], CSV_HEADERS.join(","));
    }

    #[test]
    fn csv_rows_are_sorted_by_timestamp_then_pid() {
        let h1 = fixture(10, "a", None, 2);
        let h2 = fixture(5, "b", None, 2);
        let mut buf = Vec::new();
        write_csv(&mut buf, [&h1, &h2]).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let rows: Vec<&str> = s.lines().skip(1).collect();
        // First two rows share timestamp 1_700_000_000_000 -> pid 5 then 10
        let pid_row_0 = rows[0].split(',').nth(2).unwrap();
        let pid_row_1 = rows[1].split(',').nth(2).unwrap();
        assert_eq!(pid_row_0, "5");
        assert_eq!(pid_row_1, "10");
    }

    #[test]
    fn empty_input_produces_only_header() {
        let mut buf = Vec::new();
        let empty: Vec<&ProcessHistory> = vec![];
        let n = write_csv(&mut buf, empty).unwrap();
        assert_eq!(n, 0);
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s.trim(), CSV_HEADERS.join(","));
    }

    #[test]
    fn missing_parent_pid_serialized_as_empty() {
        let h = fixture(1, "x", None, 1);
        let mut buf = Vec::new();
        write_csv(&mut buf, [&h]).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let row = s.lines().nth(1).unwrap();
        let cols: Vec<&str> = row.split(',').collect();
        // parent_pid is column index 4
        assert_eq!(cols[4], "");
    }

    #[test]
    fn present_parent_pid_serialized_as_number() {
        let h = fixture(9, "x", Some(42), 1);
        let mut buf = Vec::new();
        write_csv(&mut buf, [&h]).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let row = s.lines().nth(1).unwrap();
        let cols: Vec<&str> = row.split(',').collect();
        assert_eq!(cols[4], "42");
    }

    #[test]
    fn export_to_csv_writes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        let h = fixture(7, "p", None, 4);
        let n = export_to_csv(&path, [&h]).unwrap();
        assert_eq!(n, 4);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("timestamp_ms,"));
        assert_eq!(contents.lines().count(), 5); // header + 4 rows
    }

    #[test]
    fn names_containing_commas_are_quoted() {
        let h = fixture(1, "weird,name", None, 1);
        let mut buf = Vec::new();
        write_csv(&mut buf, [&h]).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains("\"weird,name\""),
            "commas in names must be CSV-escaped: got {s}"
        );
    }
}
