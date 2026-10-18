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
target/release/flightdeck-gpui --recording /path/to/session.recording --live-check --seconds 60
target/release/flightdeck-gpui --recording /path/to/session.recording --live-soak --seconds 900
```

Timed interactive live windows accept `--seconds 1..1800`. Run
`target/release/flightdeck-gpui --recording /path/to/session.recording --seconds 900`
to observe a full fifteen-minute history interval in the window, or use `1800` to
observe a second interval. Captured reference windows and `--live-check` retain
their 300-second maximum.

`--live-check` exercises live routes, selection identity, CPU intervals and App history
endpoints. It also rejects an invalid recording from Computer, reads the original
observation again, then reopens the actual recording and checks serialized copy
content. It reports scalar and App history coverage separately; these model probes
do not exercise the native picker, system clipboard, IME or VoiceOver. `--live-soak` runs
the same Overview worker headless for 5–1800 seconds (default 900), reporting distinct
publications, observation gaps, errors and cache accounting. It stops/drops the worker,
then waits five seconds before exiting; this does not prove a blocked worker joined.
Neither mode opens a window or measures rendering performance.

`--start-collector` writes new observations. The default recording uses the shared
`flightdeck` ring; an explicit `--recording PATH` starts an isolated ring. Only one
collector can own a ring. The app sends SIGTERM only to the collector it
started; attaching to another collector does not give it shutdown ownership.
Build the root workspace's release `collector` and `persister` first to use this
option. `--bundle` copies those helpers when they exist.
Xcode and its command-line tools are required. GPUI and its macOS backend are pinned
to upstream revision `56934ec0560bce8929025497411de7f5322fb6ca`. The direct macOS
backend includes AccessKit and avoids resolving unused platform backends. It uses
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

Open recording uses the native folder picker. Current readings remain available
until the chosen recording produces its first snapshot; cancellation or a failed
read keeps the current recording. Opening from Computer validates the recording
through Overview and lands there on success. Time entry accepts a Unix second or local
`HH:MM:SS` within the loaded history; Enter inspects that second. Copy reading copies
the selected observation, expanded raw row or selected App details as JSON, preserving
exact counter strings and nulls. Native menus support closing the window and quitting.

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

The live CPU view includes the same logical-CPU intervals and 20-segment meters as
Swift. App tables include measured executable subtotals with explicit unavailable
and overflow states. The schematic uses the shared chassis and rotor assets,
anchored callouts, illustrative airflow and explicit QA simulation. Its 30 Hz clock
suspends while inactive or offscreen. The macOS text backend is enabled and checked
at startup; the earlier blank-text builds are unsuitable performance baselines.
Overview defers the schematic until its first real snapshot, avoiding artwork
initialization while the charts are still loading.

Live controls expose accessibility roles, labels, stable identifiers and chart-step
actions through the macOS adapter. VoiceOver, native input and pixel verification
still require an interactive check; API integration alone does not establish parity.
Saved queries, every diagnostic detail and exact web chart smoothing remain outside
this prototype. App histories contain observations
seen by the reader session. Missing sensors stay unavailable. Captured Overview
follows the web design's three-column panels; live Overview follows Swift's
two-column panels. These are different workloads and must be measured separately.

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

The legacy `--scrub` workload changes the selected captured sample on each requested
animation frame. `--animate --hardware` animates the visible schematic.
Their printed percentiles measure CPU-side view-render intervals after ten warmup
renders. They are not GPU completion, display presentation, input latency, or
proof that every frame met a deadline. Neither workload is an equivalent Swift
benchmark until the same update/input pattern is run there.

Use the replay harness for matched captured-model workloads. It builds a separate
AppKit window hosting the committed Swift captured views, with replay hooks in its
private source copy. The normal SwiftUI WindowGroup shell is excluded to make
command-line startup deterministic; this is a renderer experiment baseline.
It copies the existing optimized viewer archive and records its hash; that
archive's source revision must be recorded separately for a live comparison.
From the repository root, build it first with
`cargo build --release --locked -p viewer --lib`. The commands below run from
`gpui-prototype/`.

```sh
cargo run --release --locked --bin native_replay_build
cargo run --release --locked --bin replay_workloads -- --output target/workloads --seconds 60
cargo run --release --locked --bin benchmark_runner -- \
  --gpui target/release/flightdeck-gpui \
  --swift 'target/native-replay-source/Flightdeck Swift Replay.app/Contents/MacOS/FlightdeckSwiftReplay' \
  --actions target/workloads/inspection-overview.json \
  --output target/inspection-results --watcher ../target/release/process_watch \
  --pairs 3 --warmup-ms 20000 --sample-seconds 30
cargo run --release --locked --bin benchmark_report -- \
  --input target/inspection-results --output target/inspection-summary.json
```

The generator writes idle Overview, 10 Hz Overview/CPU inspection, and visible
hardware plans. Selecting `hardware.json` enables GPUI animation automatically;
Swift animates the visible schematic by default. Plans default to 30 seconds;
`--seconds 30..600` sets their duration. The
runner alternates order, runs one app at a time, checks binary hashes, retains raw
process observations and requires equal scheduled/effective action traces. Use a
new output directory for each run. Schema 2 timestamps completed CPU model actions;
schema 1 records only batch-dispatch timing. Neither measures presentation. Verify
visible content, window dimensions and scale separately before treating the
resource samples as a fair rendering comparison.

Hardware replay uses a 1272-point GPUI window to match the assembly image size in
the 1320-point Swift helper. Normal windows retain their existing width. Both helpers
report stage paint callback counts and actual animation eligibility. Reject runs
where either window becomes inactive or the assembly is clipped; those counters
measure CPU paint work, not display presentation.

The report retains raw-log paths and
descriptive median/minimum/maximum statistics; it does not supply confidence intervals.

The 240 cached rotor orientations preserve the existing artwork through GPUI image
primitives. Their CPU pixels occupy 41,533,440 bytes, plus chassis pixels and GPU
atlas storage. Generation is timed on first schematic paint. This cost belongs
in the prototype's memory and startup results.
Live mode enables schematic animation by default, suspending its clock when
inactive or outside the painted viewport. Its visible control can pause it.

[First measurements and their limits](PERFORMANCE.md) include the raw process
observations, render summaries and binary hashes.
[Live verification and parity limits](LIVE.md) cover the shared backend prototype.
[Current evaluation](EVALUATION.md) records the font fix, repeated measurements,
implementation differences and remaining verification gaps.

A performance verdict needs repeated matched workloads, including animation and
interaction and the shared backend. Idle results alone can establish
feasibility or expose an idle cost; they cannot prove one framework universally
faster. The existing stack research explicitly records that its Swift choice was
a product-fit/maintenance judgment, without comparative runtime measurements.

Design provenance: `backlog/evidence/TASK-20/native_dashboard_v30.html`, SHA-256
`902254bcd038294d0afeef763cdab1d07bdedbfa9a5cf22ccd35e4ca708ac97e`.
The resources in this directory are copies of the existing captured reference,
so subsequent native-app changes cannot change this experiment's input.
