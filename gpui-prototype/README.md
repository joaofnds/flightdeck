# Flightdeck GPUI experiment

A separate Rust/GPUI prototype with live readings from the same Rust `LiveSession`
backend as Swift. Its visual reference is the approved web prototype v30. The
captured reference remains available with `--reference`. The Cargo workspace,
lockfile, resources and build output stay in this folder; the backend is a local
path dependency. This experiment does not change the Swift app or recording format.

```sh
cd gpui-prototype
cargo build --release --locked --bin flightdeck-gpui
cargo run --release --locked --bin flightdeck-gpui -- --bundle
open 'target/Flightdeck GPUI.app'
```

The default launch attaches to the recording at
`~/Library/Application Support/Flightdeck/flightdeck.recording`. Run the collector
through Swift, or explicitly start a collector owned by this prototype:

```sh
target/release/flightdeck-gpui --start-collector
target/release/flightdeck-gpui --recording /path/to/session.recording
target/release/flightdeck-gpui --recording /path/to/session.recording --live-check --seconds 30
```

`--start-collector` writes new observations. The default recording uses the shared
`flightdeck` ring; an explicit `--recording PATH` starts an isolated ring. Only one
collector can own a ring. The app sends SIGTERM only to the collector it
started; attaching to another collector does not give it shutdown ownership.
Build the root workspace's release `collector` and `persister` first to use this
option. `--bundle` copies those helpers when they exist.
Xcode and its command-line tools are required. The pinned GPUI 0.2.2 uses
`runtime_shaders`, which compiles Metal shaders during startup instead of requiring
Xcode's separate Metal compiler component. Startup measurements include that cost;
steady-state measurements begin after warmup. No toolchain or hooks are installed.

Live mode follows Swift's Overview, CPU & processes, Apps, Memory, GPU, Energy,
Network, Computer and Recorded readings destinations. Click a chart to pause at a
second; use time arrows, same-second publication choices, and Resume live. Expand
raw rows to read exact stored values, page through raw/App/process results, and
select an App to inspect its observed histories and process instances. Executable
groups filter the loaded process page; All process groups clears that filter. Computer
reads current host inventory on entry and Refresh. The schematic is below Overview.

The eleven captured destinations retain sample scrubbing, legend toggles, app and
process selection, help and the captured schematic:

```sh
target/release/flightdeck-gpui --check
target/release/flightdeck-gpui --reference --smoke --seconds 5
target/release/flightdeck-gpui --reference --scrub --seconds 20
target/release/flightdeck-gpui --reference --page cpu --minutes 60 --scrub --seconds 20
target/release/flightdeck-gpui --reference --hardware --animate --seconds 20
```

Rendering uses GPUI's Metal backend, canvas paths and a virtualized list of panel
rows. Live queries run on a dedicated worker with capacity-one request/result
mailboxes, cancellation generations, 512-row pages and the backend's bounded cache.
A 250 ms main-thread timer takes completed results; it does no recording I/O.
Paused inspection stops worker polling. Live context holds at most 4096 points over
900 seconds, uses actual wall-clock spacing and splits lines by run, boot and segment.
Null readings stay gaps. Captured histories retain their 121 samples.

This is a prototype. Saved queries, full native accessibility, Swift's executable
group subtotals, its new per-CPU interval presentation, every diagnostic detail and
exact web chart smoothing remain outside it. Executable groups show page-scoped
instance counts; app histories and totals retain their app scope. Fan blades use
simplified vectors; web airflow particles and schematic
QA overrides are not ported. App histories contain only observations seen by the
reader session. Missing sensors are explicit and never filled from captured data.
These differences matter when interpreting performance results.

## Measurement contract

Use optimized binaries with a visible foreground window. For captured comparisons,
use the same captured hour, last 15-minute range and selected sample 120. For live
comparisons, use one collector, the same recording and page, equal warmup/history
length and the same update cadence. Record actual window dimensions,
display scale, hardware, binary hashes, background activity and duration. Allow a
warmup before measuring idle. Alternate the implementation order on repeat runs.
Compare process CPU as a percentage of one core, physical footprint separately
from RSS, and wakeup deltas. Do not apply the collector's 5% budget to the UI.

`process_watch` in the main repository provides the same external process
observation for either app. It never changes the measured app. Build it separately
if needed with `cargo build --release -p viewer --bin process_watch` from the root.
Run only one measured app at a time and keep its window exposed. A zero CPU delta
is resolution-limited; it does not establish zero resource use.

The `--scrub` workload changes the selected captured sample on each requested
animation frame. `--animate --hardware` animates the visible simplified fans.
Their printed percentiles measure CPU-side view-render intervals after ten warmup
renders. They are not GPU completion, display presentation, input latency, or
proof that every frame met a deadline. Neither workload is an equivalent Swift
benchmark until the same update/input pattern is run there.

[First measurements and their limits](PERFORMANCE.md) include the raw process
observations, render summaries and binary hashes.
[Live verification and parity limits](LIVE.md) cover the shared backend prototype.

A performance verdict needs repeated matched workloads, including animation and
interaction and the shared backend. Idle results alone can establish
feasibility or expose an idle cost; they cannot prove one framework universally
faster. The existing stack research explicitly records that its Swift choice was
a product-fit/maintenance judgment, without comparative runtime measurements.

Design provenance: `backlog/evidence/TASK-20/native_dashboard_v30.html`, SHA-256
`902254bcd038294d0afeef763cdab1d07bdedbfa9a5cf22ccd35e4ca708ac97e`.
The resources in this directory are copies of the existing captured reference,
so subsequent native-app changes cannot change this experiment's input.
