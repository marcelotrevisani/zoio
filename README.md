# zoio

Cross-platform (macOS / Linux / Windows) process resource monitor written in Rust.

`zoio` lets you pick running processes via a checkbox list **or** match them with regex patterns, then watches their CPU, memory, disk I/O, and network I/O in real time. All matched subprocesses are followed automatically. Metrics are drawn as four live plots (one per resource) that can be viewed per-process, combined, or grouped. When you stop the session, the full capture is exported to CSV.

## Features

- **Process discovery** — live checkbox list of every running process (searchable by name or pid).
- **Regex filters** — add one or more patterns; any matching process (and optionally its child tree) is tracked.
- **Per-process metrics** — CPU %, resident memory, disk I/O rate, network I/O rate, sampled independently.
- **Real-time plots** — four separate plots (CPU / Memory / Disk / Network) rendered with `egui_plot`.
- **View modes** — Individual (one line per process), Combined (aggregate of all), Grouped (aggregate per user group).
- **Groups** — bundle the currently-selected PIDs into a named group and plot it as one aggregated series.
- **Start / Stop** — explicit session control; stopping preserves the captured data for inspection and export.
- **CSV export** — flat table with `timestamp_ms, elapsed_secs, pid, name, parent_pid, cpu_percent, memory_bytes, disk_bytes_per_sec, net_bytes_per_sec`.

## Build & run

```bash
cargo run --release
```

## Tests

```bash
# unit tests + integration tests + property tests
cargo test

# unit tests only (no child process spawning)
cargo test --lib
```

The integration suite under `tests/` spawns real child processes (`sleep` on unix, `cmd` on Windows) and verifies that `zoio` can discover them, sample their metrics, and round-trip them through CSV.

## Architecture

```
src/
├── lib.rs              # public API re-exports
├── main.rs             # eframe window bootstrap
├── app.rs              # AppState + ZoioApp (egui rendering)
├── process_monitor.rs  # sysinfo-backed discovery + sampling
├── process_filter.rs   # regex FilterSet
├── metrics.rs          # MetricKind, MetricSample, ProcessHistory
└── csv_export.rs       # CSV writer
tests/
├── monitor_flow.rs     # end-to-end: spawn → monitor → CSV
└── csv_properties.rs   # property-based CSV invariants
```

The core pipeline (`AppState`) has no egui dependency and is fully headlessly testable. The `ZoioApp` type wraps it with the eframe rendering layer.

## Platform notes

- Per-process **network** counters are not uniformly exposed by `sysinfo` across platforms. On platforms where they are unavailable, the network series reports `0.0`. Disk, CPU, and memory work everywhere `sysinfo` does.
- On Windows, accurate CPU sampling requires at least two refreshes; the first reported value is `0.0`.
