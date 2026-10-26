# Window resume performance

The [architecture guide](architecture.md) describes the process boundaries, reader
ownership and window-state decisions. This report records their measured costs and
the protocol for reproducing them.

Live Overview resumes through one reader. When the cached period is sufficient and
the registered ring still covers every unread publication, the Store retains its
machine and App predecessors and consumes that bounded delta. A gap beyond ring
retention, a changed source, or a wider uncached period reloads the selected saved
period before publishing a complete View. A completed pinned window keeps its
selection and leaves live ingestion parked.
If the writer overwrites an admitted checkpoint during consumption, the Store
withholds that View and switches once to the selected-period reload.

Overview waits for the Store instead of asking the legacy page reader to load the
same period. Initial bootstrap, incomplete histories, other pages and unavailable
Store data retain the page-reader path. Pages other than Overview park the unused
Store. A cold restore validates the complete saved predecessor minute, including
refused samples, but constructs no discarded presentation objects for its ticks.

## Measurements

Measured on 2026-10-26, Apple M5 Pro, macOS 27.0.1 (26A434), release builds with six
Polars compute threads and one asynchronous thread. The frozen real recording held
7,882 sealed windows and a 120-publication live ring; the current 1,080 observations
held 498,050 App rows. Both builds read the same immutable files, dictionary bytes,
raw counters, run identities and endpoint, publication 12,671.

Native windows were explicitly shown and activated at 1,320 × 900 logical pixels,
display scale 2. Automation started Animate fans off; no hardware reveal was requested
and scrolling stayed at the top. The recorded hardware animation state was inactive.
macOS Reduce Motion was not recorded, and background activity was not controlled.
The primary matrices ran baseline then optimized; reverse-order checks are recorded
below. Archived measured executables have these SHA-256 identities:

| Executable | SHA-256 |
| --- | --- |
| Native baseline | `a27c3f289fb97676422db0be404b19aecd01f09fdd88e4d96cecc2a8baf08f11` |
| Native optimized | `f02da685c5fb7b4f616bd4abe723dc79c2320d0f20aecfd15af3c4389aeca0c6` |
| Store baseline | `d62bdefd45cfa697cd761046362ef14d017bca5fbf2bc83a4ce5070319a56723` |
| Store optimized | `7ecb74f2a2ef01038aa41b59fe843d7234cc2f17b321e31dff12e776dfb9b5ea` |
| Native final repeat | `1306137d40f8b361ab35dd046d62358089873e72dc7da04ccd4692e3a1f57e32` |

The baseline uses production behavior from `a68f115` with the same opt-in measurement
code. The commit `measure live window resume through the native frame` supplies that
instrumented baseline; its successor contains the optimizations. Build each revision
in turn and preserve the release executables to compare them against one fixture owner.

Native latency runs from the production visibility callback through the next CPU
frame callback after a fresh, drawable Store View reaches Overview. It includes UI
scheduling and projection. It is an upper bound on CPU frame completion, excluding
GPU display latency. CPU time sums all process threads. Seven warm restores follow
one discarded warmup for each range. The harness verifies ingestion has parked before
each show; it never forces a draw while awaiting the measured frame.

| Selected range | Warm native frame, before → after (median ms) | Process CPU, before → after (median ms) |
| --- | ---: | ---: |
| 5 minutes | 172.1 → 19.5 | 507.7 → 10.3 |
| 15 minutes | 209.9 → 23.8 | 787.0 → 16.2 |
| 60 minutes | 264.3 → 23.7 | 1,093.6 → 17.8 |

A reverse-order repeat with the final implementation, seven warm 15-minute restores
per build, measured 21.5 ms and 11.7 ms CPU for optimized, then 207.0 ms and 758.4 ms
CPU for baseline. The final repeat includes the post-admission overwrite recovery and
reopen scheduling corrections. Foreground loss or incomplete initial histories invalidate
a run; the repeat waits for complete initial histories before its discarded warmup.

These warm restores have no new publications. A second experiment starts each fresh
process behind the frozen endpoint, parks ingestion, then reveals 1,800 publications.
This exceeds ring retention and requires the selected-period reload. Checkpoints hide
whole sealed minutes, so the initial displayed endpoint is about 30 minutes behind.
All measured restores reached the captured endpoint without error. Three independent
processes per build and range give these medians:

| Selected range | Native frame after long gap, before → after (ms) | Process CPU, before → after (ms) |
| --- | ---: | ---: |
| 5 minutes | 183.5 → 143.3 | 638.5 → 305.9 |
| 60 minutes | 282.8 → 237.1 | 1,375.6 → 841.5 |

The Store-only gated benchmark separates ingestion from UI and the legacy reader.
For a covered 30-publication gap, median wall time falls from 147.6 to 28.3 ms with
five minutes selected, and 240.2 to 30.0 ms with an hour selected. The corresponding
CPU times fall from 308.5 to 27.6 ms and 856.6 to 28.9 ms. For the 1,800-publication
gap, wall time falls from 145.7 to 128.5 ms and 239.2 to 220.4 ms respectively.

Warm native restores submit zero page-reader requests, versus one before. A pinned
native restore submits zero requests and keeps the Store inactive and paused. The
Store's footprint stays between 248.1 and 248.3 MB over 100 warm restores, with a
248.3 MB lifetime peak. Warm native lifetime peaks across the three ranges reach
899 MB, versus 1,317 MB before. Cold hour restores do not improve peak footprint:
the three-process maximum is 1,152 MB versus 1,100 MB, while the retained presentation
and replacement overlap. No lower cold-memory peak is claimed.

The files were filesystem-cache warm: measured disk-read bytes were zero. These
numbers establish neither cold-storage performance nor a percentile latency target.
They are local observations on a busy development Mac, not timing thresholds for CI.

## Profiling and reproduction

`sample` on the original active Store found 337 stack samples under
`recording::terminal`, including 330 under its full payload decoder. That path rebuilt
saved presentations merely to retain a raw predecessor. The optimized warm native
profile shows UI projection work with no saved-terminal reconstruction; the request
counter independently confirms the duplicate reader is idle. Profiling runs are
separate from the timing samples.

Build with the same optimization level and pools:

```console
cargo build --locked --release -p viewer --bin resume_bench
cargo build --locked --release --manifest-path desktop/Cargo.toml --features automation \
  --bin flightdeck --bin flightdeck-control
```

Keep a fixture owner running on the same filesystem as the source recording:

```console
target/release/resume_bench --freeze RECORDING --directory FIXTURE
```

Its bounded stdin protocol accepts `checkpoint N`, `advance`, and `quit`. Capture links
immutable committed files, clones and validates private shared-memory rings, and
rewrites fixture registry names and the fixture-owner pid while preserving run identities.
It never signals the original collector or
changes original recording contents. Drop restores hidden fixture markers and unlinks
only its own shared-memory objects. Disk links remain for inspection; capture requires
an absent target and refuses an unstable source or a gap before the ring generation.

For a Store sample, send `checkpoint 30` or `checkpoint 1800` to the fixture owner,
then run the chosen archived build:

```console
resume_bench --recording FIXTURE --minutes 5 --iterations 1 --gated
```

Wait for its `paused` JSON record. Send `advance` to the fixture owner, then `advance`
to the benchmark's stdin. Its timing starts after the gate, so fixture manipulation
is excluded. Start a fresh benchmark process for each nonzero-gap sample. Without
`--gated`, the tool emits startup, warmup and up to 100 repeated-resume records.

For native samples:

```console
flightdeck --recording FIXTURE --attach-live --control SOCKET --minutes 15 --seconds 120
flightdeck-control --socket SOCKET --command show --command profile-resume \
  --command 'range 5' --command profile-cycle
```

`--attach-live` exists only in automation builds and attaches the actual live readers
without starting a collector. `profile-cycle` waits for the hidden Store to park,
shows the real window and returns after a new measurement epoch completes. For a long
gap, finish initial histories, minimize, and inspect `state` until the window is hidden
and the Store is inactive and paused. Advance the fixture only then, and issue
`profile-cycle`. Check the returned endpoint and request count. Polling `state` while
the measured frame is pending forces draws and invalidates the timing.

For stack profiling, attach `/usr/bin/sample PID 3 1 -mayDie -file OUTPUT` while a
separate control client runs repeated cycles. Use those samples for attribution, not
latency comparisons. The root and desktop checks exercise the frozen fixture, ring
coverage, widening, identity changes, hidden/reopened single-reader behavior, pinned
state, predecessor rates, malformed input, cancellation and presentation bounds.
