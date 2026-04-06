//! Property-based tests for the CSV export pipeline.

use proptest::prelude::*;
use zoio::csv_export::{write_csv, CSV_HEADERS};
use zoio::metrics::{MetricSample, ProcessHistory, ProcessIdentity};

fn build_history(pid: u32, samples: Vec<(f64, f32, u64)>) -> ProcessHistory {
    let id = ProcessIdentity::new(pid, format!("proc{pid}"), 0);
    let mut h = ProcessHistory::new(id, samples.len().max(1) + 1);
    for (t, cpu, mem) in samples {
        h.push(MetricSample {
            elapsed_secs: t,
            wall_clock_ms: (t * 1000.0) as i64,
            cpu_percent: cpu,
            memory_bytes: mem,
            disk_bytes_per_sec: 0.0,
            net_bytes_per_sec: 0.0,
        });
    }
    h
}

proptest! {
    #[test]
    fn csv_row_count_equals_total_samples(
        counts in proptest::collection::vec(0usize..10, 1..5)
    ) {
        let mut histories = Vec::new();
        let mut total = 0;
        for (i, n) in counts.iter().enumerate() {
            let samples: Vec<(f64, f32, u64)> =
                (0..*n).map(|k| (k as f64, k as f32, k as u64)).collect();
            total += n;
            histories.push(build_history(i as u32 + 1, samples));
        }
        let mut buf = Vec::new();
        let written = write_csv(&mut buf, histories.iter()).unwrap();
        prop_assert_eq!(written, total);

        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        prop_assert_eq!(lines.len(), total + 1); // header + data rows
        prop_assert_eq!(lines[0], CSV_HEADERS.join(","));
    }

    #[test]
    fn csv_rows_always_sorted_by_timestamp(
        n in 1usize..20
    ) {
        // Build a single history with random-order timestamps.
        let mut samples: Vec<(f64, f32, u64)> =
            (0..n).map(|i| ((n - i) as f64, i as f32, i as u64)).collect();
        // Shuffle by rotating (deterministic, no rng dependency).
        samples.rotate_left(n / 2);
        let h = build_history(1, samples);

        let mut buf = Vec::new();
        write_csv(&mut buf, [&h]).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let timestamps: Vec<i64> = s
            .lines()
            .skip(1)
            .map(|l| l.split(',').next().unwrap().parse().unwrap())
            .collect();
        let mut sorted = timestamps.clone();
        sorted.sort();
        prop_assert_eq!(timestamps, sorted);
    }
}
