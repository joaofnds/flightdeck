# First GPUI measurements

**Superseded measurements.** The GPUI builds measured below disabled the `font-kit`
feature. On macOS GPUI 0.2.2 then selects `NoopTextSystem`: glyphs have zero raster
bounds and no application text is painted. These numbers omit a material part of
the intended UI workload and cannot evaluate GPUI against Swift. The raw evidence
is retained as a record of those binaries, not a performance baseline. Current
builds enable `font-kit` and fail startup if system glyph metrics are placeholders.

The later comparison used the scheduled replay harness retained in Git history
before the Swift client retirement. Current usage is in [README.md](README.md). CPU action acceptance and process resource usage remain
separate from GPU completion and displayed-frame latency.

The prototype's view-render callbacks run roughly 120 times per second while
inspecting the captured Overview. Its CPU-side view loop keeps that cadence.
This does not establish GPU presentation or input latency, or
which implementation is faster under matched interaction.

After a twenty-second settling period, one fifteen-second idle observation gave:

- GPUI: **0.225% of one CPU core**, **192.8 MiB physical footprint**, 103.1 MiB RSS.
- Existing Swift reference app: **0.134% of one core**, **109.1 MiB physical
  footprint**, 176.8 MiB RSS.
- Interrupt wakeups were approximately **120.3/s** for GPUI and **0.46/s** for
  Swift. The prototype itself requests no idle animation frames. This is a
  measured idle wakeup cost worth investigating in GPUI's platform loop.

The GPUI build therefore had about 84 MiB more physical footprint in this pair.
The absolute CPU difference was about 0.09 percentage points of one core. RSS
alone would reverse the memory ranking, which is why physical footprint is
reported separately. These are short observations on a busy development machine,
not confidence intervals or a general framework verdict.

GPUI-only active observations used ten seconds of warmup and fifteen seconds of
external process observation during a twenty-eight-second run:

- Overview scrub: **33.85% of one core**, **297.4 MiB** final physical footprint;
  3,352 view renders in 28.00 seconds. Render interval p50/p95/p99:
  **8.33 / 9.09 / 9.31 ms**.
- Visible simplified fan animation: **17.40% of one core**, **362.6 MiB** final
  physical footprint; 3,357 view renders in 28.01 seconds. Render interval
  p50/p95/p99: **8.33 / 9.24 / 9.33 ms**.

Those percentiles measure CPU-side view-render intervals after ten warmup
renders. They do not measure GPU completion, displayed frames or input response.
The animation omits the web design's airflow particles and uses simplified fan
geometry. Neither active workload has a matched Swift measurement here, so these
figures cannot rank active performance between the implementations.

## Conditions and evidence

Measured on 2026-10-17: Apple M5 Pro, Mac17,8, 18 CPU cores, 48 GiB RAM, macOS
27.0.1; AC power. The built-in display reported 1728 × 1117 points at 120 Hz,
3456 × 2234 pixels. GPUI reported a 1320 × 901-point viewport at scale 2. Its
requested window size was 1320 × 900, matching the native source's default size.
Actual native window size, occlusion and input were not independently verified.
Desktop automation returned “Computer Use was not approved to use Flightdeck
GPUI” and also refused the Swift app, so pixel/input QA was unavailable.
Other sessions and applications remained active; background load was not held
constant. The load average before an earlier pair was 4.65 / 4.46 / 4.24.

The Rust binary is an optimized release build from Rust 1.99.1, with overflow
checks and panic abort enabled. CI verification separately passed under Rust
1.95.0. The Swift baseline was a private copy of the existing `target/Flightdeck.app`
bundle launched with `--reference`, without rebuilding or changing the other
session's sources. The repository's native builder uses optimized Swift/Rust;
the copied binary's precise source revision and build flags were not independently
recovered. Its SHA-256 identifies the baseline actually observed.

Both apps used the same captured reference hour, final fifteen-minute range and
selected sample 120. Their visible scenes are approximate ports with different
rendering implementations, not identical draw-command workloads. No collector or
historical query workload is included. GPU time, energy, hidden-window behavior,
startup-to-first-frame latency and native accessibility were not compared.

[Raw process samples, render summaries and binary hashes](evidence/) are retained.
`settled-*-idle.jsonl` contains the twenty-second-warmup pair. `measured-*-idle.jsonl`
contains an earlier three-second-warmup pair that remained busy for part of the
observation and is excluded from the steady-state summary. The cause of that
settling work was not diagnosed. `summary.json` derives CPU percentages from
counter deltas divided by measured elapsed time, divides bytes by 1,048,576 for
MiB, and reports the last observed footprint. No samples were silently dropped.

The evidence supports keeping GPUI as a feasible candidate and measuring it
further. It shows higher idle physical footprint and wakeups in this build;
it does not prove GPUI intrinsically slower than SwiftUI/AppKit. A decision needs
repeated matched input/animation workloads against an identified native source
revision, including the full schematic and the shared backend.

## Repeating an idle observation

Build the app and the repository's `process_watch` as described in the README.
From `desktop/`, run this while leaving the measured window exposed:

```sh
mkdir -p measurements
target/release/flightdeck --reference > measurements/app.log 2>&1 &
measured_pid=$!
sleep 20
../target/release/process_watch --pid "$measured_pid" --seconds 15 > measurements/idle.jsonl
kill -TERM "$measured_pid"
```

Record the binary hash, source revision, window size, visibility, range and input
state. Repeat the same workload before interpreting small differences. A live
observation is a different workload and must identify its recording and producer.
