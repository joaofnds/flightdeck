# Flightdeck desktop

The Rust/GPUI desktop client uses the shared `viewer::LiveSession` recording
backend. Its visual reference is the approved web prototype v30. The captured
reference remains available with `--reference`. The UI has its own Cargo workspace
and lockfile; the recording backend is a local path dependency.

```sh
just dev
```

`just dev` builds the helpers and the app, packages them, checks the bundle and opens it,
with the tools `mise.toml` pins, as the repository README describes. The bundle contains
the app, required collector and persister executables, and artwork; missing helpers fail
packaging. It can be moved outside the checkout.

Default launch uses `~/Library/Application Support/Flightdeck/flightdeck.recording`.
It attaches to an advancing existing collector or starts its own when collection
is absent. It attaches only to a collector built from the same collector code, and
otherwise fails with both builds named. A ring states its own layout, which every reader
checks, so an app opening a recording another build wrote reads it only where the
layouts agree. A collector counts as running only while it holds the recording's lifetime
lock, which macOS releases when it exits, so a pid given to another process never
passes for it. A launch on any page, Computer included, starts collection the same way.
A recording supplied with `--recording PATH` opens as Recorded and reads on navigation or Refresh, without idle polling. Add
`--start-collector` to collect into that root and follow Live readings. When a
collector of the same build already collects there, the app attaches to it rather than
starting a second one, on whichever page it opens. The following commands run from `desktop/`:

```sh
target/release/flightdeck --start-collector
target/release/flightdeck --recording /path/to/session.recording
target/release/flightdeck --recording /path/to/session.recording --live-check --seconds 60
target/release/flightdeck --recording /path/to/session.recording --live-soak --seconds 900
```

The Live control in a Recorded window follows new readings from that recording
through the existing reader. It does not start a collector or acquire shutdown ownership.

A build with the `automation` feature can be driven without the screen. With
`--control SOCKET` the window opens hidden and answers one command
per line on that Unix socket with one JSON line. `flightdeck-control` sends them:

```sh
cargo +1.98.0 build --locked --features automation
target/debug/flightdeck --start-collector --control /tmp/flightdeck.sock &
target/debug/flightdeck-control --socket /tmp/flightdeck.sock --command help
target/debug/flightdeck-control --socket /tmp/flightdeck.sock \
  --command 'navigate battery' --command 'move 500 420' \
  --command state --command 'screenshot /tmp/battery.png' --command quit
```

`state` reports the route, the selected and hovered seconds, the live status, the
viewport, the scroll position, Settings state and animation counters. `show` explicitly
shows and focuses this window; `minimize` minimizes it. `rows` lists the page's rows, which
`reveal ROW_KEY` scrolls to the top. `hovers` reports the latest 64 chart hovers, oldest
first, each with its page, its second and `complete_ms`, the time the app measured from
handling the pointer move to the page holding that second's complete readings. It is null
for a hover that ended, or whose read failed, before then. `count` counts every hover since
launch. The socket answers at the window's own pace, so its round trip cannot time a hover.
Pointer commands take window coordinates in points. A screenshot renders GPUI's frame
offscreen at its scale factor. Native Metal fan animation is a separate surface and needs
a visible window capture. The socket screenshot waits up to five seconds for the readings and
reports in `settled` whether they arrived. Builds without the feature refuse `--control`.

Timed interactive live windows accept `--seconds 1..1800`. Run
`target/release/flightdeck --seconds 900`
to observe a full fifteen-minute history interval in the window, or use `1800` to
observe a second interval. Captured reference windows and `--live-check` retain
their 300-second maximum. When its time is up a timed live window prints one JSON line of
what it saw, with the `hovers` report under that name, so a build without the `automation`
feature also shows how long the hovers made in it took.

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
`flightdeck` ring; an explicit `--recording PATH` starts an isolated ring. Once that
collector exits and its readings are saved, the app removes the isolated ring and its lock
file, so a scratch recording leaves no ring behind. Only one collector can own a ring. Startup and child-exit failures appear with a Retry
control. The app sends signals only to the collector it started; attaching to
another collector does not give it shutdown ownership, so quitting leaves that collector
running. A live launch keeps running
once its window closes, with no Dock icon, so collection goes on and no reading is
missed. Its item in the menu bar brings the window back, as does opening the app
again, and a right click or Control-click on the item opens a menu with Quit. A
launch that opens a recording read-only, runs for `--seconds` or listens on
`--control` still quits with its last window. Quitting stops owned collection and
waits for its final save. The save budget is 125 seconds, followed by a two-second
forced-exit budget.
Xcode and its command-line tools are required. GPUI and its macOS backend come from a
fork, https://github.com/joaofnds/zed on branch `flightdeck-gpui`, at the revision
`Cargo.toml` pins. The branch is upstream revision
`56934ec0560bce8929025497411de7f5322fb6ca` with commits that make two changes. The first
pauses a window's display link once GPUI has asked for no frame for four frames, where
upstream ticks it at every vsync while the window is visible, 120 times a second on a
ProMotion display, though a page changes once a second. The second keeps each font's
sized copy, where upstream makes a new one for every run of text it shapes. On an M5 Pro
at rest, Overview at the hour range spent 14.7 to 18.4 ms of CPU a second on upstream GPUI
and 9.0 to 12.9 with both changes. The branch is never rewritten, since the pin names a
commit on it, so moving to a later upstream revision means carrying the changes onto a
new branch and pinning that. The fork can go once upstream carries both. The direct macOS
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
resident memory, footprint, process power, disk, wakeup, page-in and thread histories
above the App totals;
those histories include captured members outside the loaded page. All process groups
is the default and shows those eight histories with the seven highest mean-CPU names
over the context plus Other. It includes captured members outside the loaded page
and clears the named selection and instance filter. Executable and process-instance
tables include wakeups per second, page-ins per second and current thread counts.
Missing or empty names retain
the filter and explain why named history is unavailable. Computer
reads current host inventory on entry and Refresh. Self-monitoring separates the
latest producer's metadata from the selected observation and reports reader losses,
saved windows and cache accounting. Refresh updates that metadata even while the
reading selection is paused. The schematic is below Overview.

Hovering a plot temporarily shows that exact observation across the page, including
Memory quantities, CPU and App details, Storage/Battery summaries, the schematic
and Copy. Leaving the plot
restores the live or paused selection. A click on a plot, or Return on a second
the arrow keys reached, pins that reading instead: every page with a time selection
then shows it and marks it with a band on each chart. A hover still shows the hovered
second across the page, and leaving the plot returns to the pinned one rather than the
live selection, until Go to latest reading, the range's reset or another recording lets
it go. Missing
readings stay unavailable. The
reader keeps each second's top Apps for the selected window, and the selected App's
processes on Apps, so a hover shows them in the frame that handles the move. A
second not kept yet, the Apps an open App picker lists, and an App too large for
its bound show a loading state while the existing reader fetches them. A newly
opened page brings the top Apps its window already keeps with its first readings,
and the CPU page its logical-CPU history.
A hover on a second not kept yet, while the page still reads its App histories,
waits for that read, which keeps the rest, rather than cancelling it with a read
of its own.
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
target/release/flightdeck --reference --hardware --seconds 20
```

Rendering uses GPUI's Metal backend, canvas paths and a virtualized list of panel
rows. Live queries run on a dedicated worker with capacity-one request/result
mailboxes, cancellation generations, 512-row pages and the backend's bounded cache.
A 250 ms main-thread timer takes completed results.
Live Overview, while it follows the newest second, draws from the store instead. The app
starts the store before its window opens and keeps it for its life. The store's thread writes
the readings into buffers it allocates when it starts, and the same timer takes the store's
newest View, which never changes while the page holds it, so a window shown again draws what
the store holds at once. While Overview draws from the View the worker rests, and a hover over
a second the View holds reads it there. A chosen second, a saved recording and every other
page read through the worker. A View older than the worker's readings is not drawn, so the
charts never show less than the worker read. During collector discovery and
startup the timer also reads bounded registration/ring-header metadata for the configured
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
Read through the worker, Overview prepares CPU, process-power and Disk App histories
together, sharing saved context work and the existing bounded History cache. Their cohorts
and failures remain independent; Resident history uses its own source path.
CPU, Memory, Energy and Storage repeat the corresponding ranking on their own pages,
beside their captured App histories. The CPU plot uses 100% for one core; process
power remains separate from system and GPU power. Energy's Process power summary
uses the same App history and inspected timestamp as its plot.
CPU and Disk App histories include independent dashed machine references. All CPU
sums the recorded logical CPUs' busy percentages; All disk I/O requires complete
whole-device read and write rates. Both follow exact observation identities and
stay outside captured App totals. Missing machine intervals remain unavailable.
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
The schematic draws the shared chassis and rotor assets still by default, with
anchored callouts and an illustrative airflow tint. Settings in the sidebar, the app menu,
or Command-comma offers **Animate fans**. The choice saves immediately for normal app
windows, including saved recordings. Timed runs and automation windows keep a session-only choice.
Each Settings control shows its default. When changed, **Restore default** resets that
setting and saves immediately, leaving the other preferences alone. Animate fans defaults to Off.
An independent Metal surface animates two full-color rotors and outgoing particles,
targeting 30 frames per second without requesting dashboard frames. Each side follows its displayed
RPM; zero or missing RPM stops that side. Motion pauses outside Overview, when clipped out,
under chart or time overlays, when the window is inactive or hidden, and with macOS Reduce
Motion enabled. A renderer failure leaves the still figure and shows an error.
The macOS text backend is enabled and checked
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
Record Animate fans, macOS Reduce Motion, schematic visibility and observed animation
activity. Match these conditions unless animation is the variable being compared.
Ordinary windows load the saved choice; timed and automation windows start with it off.
Compare process CPU as a percentage of one core, physical footprint separately
from RSS, and wakeup deltas. Do not apply the collector's 5% budget to the UI.

`process_watch` in the main repository provides the same external process
observation for the desktop app. It never changes the measured app. Build it separately
if needed with `cargo build --release -p viewer --bin process_watch` from the root.
Run only one measured app at a time and keep its window exposed. A zero CPU delta
is resolution-limited; it does not establish zero resource use.

The legacy `--scrub` workload changes the selected captured sample on each requested
animation frame. Its printed percentiles measure CPU-side view-render intervals after ten warmup
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

The rotor is held as two images, the left fan's and its mirror for the right one.
Their CPU pixels occupy 346,112 bytes, plus chassis pixels and GPU atlas storage.
Generation is timed on first schematic paint. This cost belongs in the
application's memory and startup results.

[First measurements and their limits](PERFORMANCE.md) include the raw process
observations, render summaries and binary hashes.
[Live verification and parity limits](LIVE.md) cover the shared backend client.
[Current evaluation](EVALUATION.md) records the font fix, repeated measurements,
implementation differences and remaining verification gaps.

A performance verdict needs repeated matched workloads, including interaction
and the shared backend. Idle results alone can establish
feasibility or expose an idle cost; they cannot prove one framework universally
faster. The existing stack research explicitly records that its Swift choice was
a product-fit/maintenance judgment, without comparative runtime measurements.

Design provenance: `backlog/evidence/TASK-20/native_dashboard_v30.html`, SHA-256
`902254bcd038294d0afeef763cdab1d07bdedbfa9a5cf22ccd35e4ca708ac97e`.
The resources in this directory are copies of the existing captured reference,
and remain fixed inputs for the captured reference.
