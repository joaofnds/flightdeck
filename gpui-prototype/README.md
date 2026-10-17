# Flightdeck GPUI experiment

A separate Rust/GPUI rendering prototype using the approved web prototype v30's
captured samples, chart definitions, colors and layout. It does not read or change
recordings, start a collector, or change the Swift app. Its Cargo workspace,
lockfile, resources and build output are independent of the recording workspace.

```sh
cd gpui-prototype
cargo build --release --locked --bin flightdeck-gpui
cargo run --release --locked --bin flightdeck-gpui -- --bundle
open 'target/Flightdeck GPUI.app'
```

Or run `cargo run --release --locked --bin flightdeck-gpui` directly.
Xcode and its command-line tools are required. The pinned GPUI 0.2.2 uses
`runtime_shaders`, which compiles Metal shaders during startup instead of requiring
Xcode's separate Metal compiler component. Startup measurements include that cost;
steady-state measurements begin after warmup. No toolchain or hooks are installed.

The eleven destinations use captured data, not current readings. Move across a
chart to inspect a shared sample; use the time arrows or 15-minute/hour controls.
Click legend entries to hide series, an app ranking to drill down, or a process
name to filter its charts. The info buttons open the captured metric explanations.
The schematic is at the bottom of Overview; `--hardware` opens it directly.

```sh
target/release/flightdeck-gpui --check
target/release/flightdeck-gpui --smoke --seconds 5
target/release/flightdeck-gpui --scrub --seconds 20
target/release/flightdeck-gpui --page cpu --minutes 60 --scrub --seconds 20
target/release/flightdeck-gpui --hardware --animate --seconds 20
```

Rendering uses GPUI's Metal backend, canvas paths and a virtualized list of panel
rows. JSON is parsed once. Only visible rows and a bounded overdraw region render;
the default idle screen requests no animation frames. Chart histories are bounded
to 121 captured samples, with gaps retained and sends/writes mirrored below zero.
Memory categories stack while installed RAM remains independent.

This is a rendering experiment. It does not implement live collection, saved
queries, full native accessibility, every diagnostic detail, or exact web chart
smoothing. The fan blades are a simplified vector rendering; web airflow particles
and schematic QA overrides are not ported. These differences must remain visible
when interpreting performance results. It is not production feature parity.

## Measurement contract

Use optimized binaries with a visible foreground window, the same captured hour,
the last 15-minute range and selected sample 120. Record actual window dimensions,
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

A performance verdict needs repeated matched workloads, including animation and
interaction, and eventually the shared backend. Idle results alone can establish
feasibility or expose an idle cost; they cannot prove one framework universally
faster. The existing stack research explicitly records that its Swift choice was
a product-fit/maintenance judgment, without comparative runtime measurements.

Design provenance: `backlog/evidence/TASK-20/native_dashboard_v30.html`, SHA-256
`902254bcd038294d0afeef763cdab1d07bdedbfa9a5cf22ccd35e4ca708ac97e`.
The resources in this directory are copies of the existing captured reference,
so subsequent native-app changes cannot change this experiment's input.
