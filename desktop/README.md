# Flightdeck desktop

The Rust/GPUI desktop client uses the shared `viewer::LiveSession` recording
backend. Its visual reference is the approved web prototype v30. The captured
reference remains available with `--reference`. The UI has its own Cargo workspace
and lockfile; the recording backend is a local path dependency.

```sh
cargo +1.98.0 build --release --locked -p collector -p persistence --bin collector --bin persister
cargo +1.98.0 build --release --locked --manifest-path desktop/Cargo.toml --bin flightdeck
desktop/target/release/flightdeck --bundle
'desktop/target/Flightdeck.app/Contents/MacOS/flightdeck' --bundle --check
open 'desktop/target/Flightdeck.app'
```

Run those commands from the repository root. The bundle contains the app, required
collector and persister executables, and artwork; missing helpers fail packaging.
It can be moved outside the checkout. Rust 1.98.0 matches CI; these commands install
no toolchain or hooks.

Default launch uses `~/Library/Application Support/Flightdeck/flightdeck.recording`.
It attaches to an advancing existing collector or starts its own when collection
is absent. Initial Computer-only entry defers automatic collection until a
recording route is opened. A recording supplied with `--recording PATH` opens as
Recorded and reads on navigation or Refresh, without idle polling. Add
`--start-collector` to collect into that root and follow Live readings. The
following commands run from `desktop/`:

```sh
target/release/flightdeck --start-collector
target/release/flightdeck --recording /path/to/session.recording
target/release/flightdeck --recording /path/to/session.recording --live-check --seconds 60
target/release/flightdeck --recording /path/to/session.recording --live-soak --seconds 900
```

The Live control in a Recorded window follows new readings from that recording
through the existing reader. It does not start a collector or acquire shutdown ownership.

Timed interactive live windows accept `--seconds 1..1800`. Run
`target/release/flightdeck --seconds 900`
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
collector can own a ring. Startup and child-exit failures appear with a Retry
control. The app sends signals only to the collector it started; attaching to
another collector does not give it shutdown ownership. Closing the final window
or quitting stops owned collection and waits for its final save. The save budget
is 125 seconds, followed by a two-second forced-exit budget. Background collection
after closing the app is a separate product feature.
Xcode and its command-line tools are required. GPUI and its macOS backend are pinned
to upstream revision `56934ec0560bce8929025497411de7f5322fb6ca`. The direct macOS
backend includes AccessKit and avoids resolving unused platform backends. It uses
`runtime_shaders`, which compiles Metal shaders during startup instead of requiring
Xcode's separate Metal compiler component. Startup measurements include that cost;
steady-state measurements begin after warmup. No toolchain or hooks are installed.

Live mode provides Overview, CPU, Apps, Memory, GPU, Storage,
Energy, Network, Battery, Computer, Self-monitoring and Recorded readings
destinations. Click a chart to pause at a
second; use time arrows, same-second publication choices, and Resume live. Expand
raw rows to read exact stored values, page through raw/App/process results, and
select an App to inspect its observed histories and process instances. Executable
rows group the loaded process page. Selecting a captured name loads its CPU,
resident memory, footprint, process power and disk histories above the App totals;
those histories include captured members outside the loaded page. All process groups
clears the named selection and instance filter. Missing or empty names retain the
filter and explain why named history is unavailable. Computer
reads current host inventory on entry and Refresh. Self-monitoring separates the
latest producer's metadata from the selected observation and reports reader losses,
saved windows and cache accounting. Refresh updates that metadata even while the
reading selection is paused. The schematic is below Overview.

Hovering a plot temporarily shows that exact observation across the page, including
Memory quantities, CPU and App details, Storage/Battery summaries, the schematic
and Copy. Leaving the plot
restores the live or paused selection. Missing readings stay unavailable; details
that need storage show a loading state while the existing reader fetches them.
Charts retain their time range and position during inspection. Historical detail
lookups use the existing capacity-one worker and submit at most once per 250 ms;
leaving the plot restores its committed request.

Open recording uses the native folder picker. Current readings remain available
until the chosen recording produces its first snapshot; cancellation or a failed
read keeps the current recording. Opening from Computer validates the recording
through Overview and lands there on success. Open waits while a previous reader
finishes. After five seconds it explains how to recover by reopening the app;
the current recording remains usable, and Open returns automatically if cleanup
finishes. This retains at most one retiring reader instead of starting more workers.
Time entry accepts a Unix second or local
`HH:MM:SS` within the loaded history; Enter inspects that second. Copy reading copies
the selected observation, expanded raw row or selected App details as JSON, preserving
exact counter strings and nulls. Native menus support closing the window and quitting.

The eleven captured destinations retain sample scrubbing, legend toggles, app and
process selection, help and the captured schematic:

```sh
target/release/flightdeck --check
target/release/flightdeck --reference --smoke --seconds 5
target/release/flightdeck --reference --scrub --seconds 20
target/release/flightdeck --reference --page cpu --minutes 60 --scrub --seconds 20
target/release/flightdeck --reference --hardware --animate --seconds 20
```

Rendering uses GPUI's Metal backend, canvas paths and a virtualized list of panel
rows. Live queries run on a dedicated worker with capacity-one request/result
mailboxes, cancellation generations, 512-row pages and the backend's bounded cache.
A 250 ms main-thread timer takes completed results. During collector discovery and
startup it also reads bounded registration/ring-header metadata for the configured
root; it does no database query or tick/event copying. Automatic attachment keeps
monitoring that producer and surfaces stopped or stalled collection through Retry,
without taking shutdown ownership. The metadata syscalls have bounded input sizes but
cannot preempt a slow filesystem. Recording queries stay on the worker.
Paused observation pages stop worker polling; live Self-monitoring continues
updating connection metadata while preserving the selected identity. Live context
holds at most 4096 points over 900 seconds, uses actual wall-clock spacing and splits
lines by run, boot and segment.
Null readings stay gaps. Captured histories retain their 121 samples.

Storage shows physical-device throughput, operations, mean I/O time and errors,
with bounded device cards and explicit partial or unavailable scans. Mirrored
writes retain positive inspected values. Disk I/O by App plots eight leaders plus
Other over the captured fifteen-minute context, beside rankings for the selected
observation. Both use jointly measured process read and write deltas, separately
from device throughput. Byte counts remain exact; small rates use kB/s or B/s.
Battery shows charge, signed power flow,
observed charging states and the selected controller details. Resource summaries,
device cards and Copy follow graph hover, including missing observations.

The live CPU view includes selected logical-CPU intervals, 20-segment meters and
900-second logical-CPU history, paged 32 CPUs at a time. History preserves actual
predecessor intervals, gaps and exact observations; the labels identify logical
scheduler CPUs. Memory shows active, wired and compressed composition against a
dashed installed-capacity guide, all twelve selected VM/swap quantities, categorical
pressure and swap traffic. Page-ins, page-outs, compression and decompression use
captured page rates. Energy includes available SoC temperature and indexed
fan histories, preserving stopped zero readings and missing channels. Recording
context includes known extent, saved-window counts, source errors and paged
same-second alternatives with full run/boot/publication identities.

Overview ranks the top ten captured Apps by CPU, resident memory, process power and
jointly measured disk I/O. Its App resource histories follow those rankings, pairing
CPU, resident memory, disk and process-power charts at viewport widths from 640 points
and stacking them below. All four retain their captured context during exact inspection;
missing intervals and individual source failures stay explicit. Per-App network and GPU
histories are not captured. Cooling and battery context follows the inspected timestamp,
with an explicit uncaptured thermal-state reading.
CPU, Memory, Energy and Storage repeat the corresponding ranking on their own pages,
beside their captured App histories. The CPU plot uses 100% for one core; process
power remains separate from system and GPU power. Energy's Process power summary
uses the same App history and inspected timestamp as its plot.
Bars show each value relative to the leading App; help explains the measured quantity.
Rankings scan all captured Apps through bounded pages, retaining ten entries per
metric. Zero, partially measured totals, overflow and incomplete membership remain
explicit. Hover inspection loads rankings for the exact observation or shows its
loading/error state. Selecting a rank opens that App at the displayed observation.

App tables include measured executable subtotals with explicit unavailable
and overflow states. Selected named process groups and selected App histories
include CPU, resident memory,
footprint, process power and disk rates. Disk charts show Read and Write in both
the headline and accessible value, with separate measured-coverage counts;
missing observations remain gaps and small nonzero rates retain useful units.
The schematic shows the selected Battery charge and signed power independently;
missing Battery readings remain unavailable. Invalid Battery input reports its
diagnostic while preserving valid CPU, GPU, installed-memory and fan readings.
The schematic uses the shared chassis and rotor assets,
anchored callouts, illustrative airflow and explicit QA simulation. Its 30 Hz clock
suspends while inactive or offscreen. The macOS text backend is enabled and checked
at startup; the earlier blank-text builds are unsuitable performance baselines.
Overview defers the schematic until its first real snapshot, avoiding artwork
initialization while the charts are still loading.

Live controls expose accessibility roles, labels, stable identifiers and chart-step
actions through the macOS adapter. VoiceOver, native input and pixel verification
still require an interactive check; API integration alone does not establish parity.
Saved queries, every diagnostic detail and exact web chart smoothing remain outside
this implementation. App histories merge retained live observations with committed
recording data, preserving exact identities and showing gaps where source or
retention limits prevent recovery. Missing sensors stay unavailable. Live Overview uses
three chart columns from 1100 points wide, two from 640, and one below 640.
Memory starts with composition and pressure, followed by Swap and paging; that
three-card band uses the same three/two/one breakpoints, keeping equal column
widths in its incomplete row. Named-process histories also use three/two/one
columns; selected-App totals use two from 640 and one below. Incomplete Apps rows
retain their declared column widths. Other live chart rows use two columns from
640 and one below it. Header controls wrap
inside narrow content. Overview includes categorical memory-pressure history.
Captured and live pages have different panel/data workloads and must be measured separately.

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
observation for the desktop app. It never changes the measured app. Build it separately
if needed with `cargo build --release -p viewer --bin process_watch` from the root.
Run only one measured app at a time and keep its window exposed. A zero CPU delta
is resolution-limited; it does not establish zero resource use.

The legacy `--scrub` workload changes the selected captured sample on each requested
animation frame. `--animate --hardware` animates the visible schematic.
Their printed percentiles measure CPU-side view-render intervals after ten warmup
renders. They are not GPU completion, display presentation, input latency, or
proof that every frame met a deadline.

The `replay_workloads` binary generates captured Overview, CPU inspection and
hardware action plans for the client's replay mode. Historical cross-framework
measurements remain in [EVALUATION.md](EVALUATION.md); their Swift helper and paired
runner are available only in Git history before the client retirement.

Allocation experiments accept a frozen 900-point real recording selection. From
`desktop/`, use fresh output filenames for each invocation:

```sh
cargo +1.98.0 build --release --locked --bin projection_bench
target/release/projection_bench --capture /path/to/session.recording --output target/selection.json
target/release/projection_bench --input target/selection.json --route memory \
  --iterations 20 --output target/memory-bits.json
target/release/projection_bench --paint-input target/memory-bits.json \
  --iterations 20 --output target/prepared-bits.json
```

The capture requires 900 saved scalar points. Compare frozen binaries using the
same input and byte-identical output transcripts. Paint counters cover production
line preparation, excluding path construction, GPU work and pixels.

To measure a selected App's five chart projections, identify its exact second,
same-second alternative and captured key bytes. From `desktop/`:

```sh
target/release/projection_bench --capture-app /path/to/session.recording \
  --second 1792344734 --alternative 0 --kind bundle \
  --bytes-hex 2f53797374656d2f4c6962726172792f436f726553657276696365732f57696e646f774d616e616765722e617070 \
  --output target/app-selection.json
target/release/projection_bench --app-input target/app-selection.json \
  --iterations 20 --output target/app-chart-bits.json
```

The example key names WindowManager; choose a key and second present in the input
recording. Capture preserves the exact selected observation and App-history envelope.
Measurement validates that envelope, performs one oracle/warmup projection, then
records wall time, process CPU and Rust allocation requests during production panel
preparation. Requested bytes are not retained memory. Querying, decoding, encoding,
UI drawing and screen presentation are outside this measurement. Each output path
must be new; the command refuses to overwrite an existing file.

The 240 cached rotor orientations preserve the existing artwork through GPUI image
primitives. Their CPU pixels occupy 41,533,440 bytes, plus chassis pixels and GPU
atlas storage. Generation is timed on first schematic paint. This cost belongs
in the application's memory and startup results.
Live mode enables schematic animation by default, suspending its clock when
inactive or outside the painted viewport. Its visible control can pause it.

[First measurements and their limits](PERFORMANCE.md) include the raw process
observations, render summaries and binary hashes.
[Live verification and parity limits](LIVE.md) cover the shared backend client.
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
and remain fixed inputs for the captured reference.
