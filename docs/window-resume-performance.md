# Window resume performance

The [architecture guide](architecture.md) describes the process boundaries, reader
ownership and window-state decisions. This report records their measured costs and
the protocol for reproducing them.

Live Overview resumes through one reader, the Store, which holds the loaded hour, the
newest 3,600 s of readings, whatever range is selected. When the registered ring still
covers every unread publication, the Store retains its machine and App predecessors and
consumes that bounded delta. After a gap beyond ring retention or a collector restart, it
reads the newest sealed minute it holds whole, to seed rates, and every minute the
recording sealed after it, before publishing a complete View. It reads the whole hour again
where it holds no whole sealed minute of that run, or where the held hour left a minute
out for any reason. A newest held second from another recording, or from a run the
registry no longer lists, is refused, and nothing is read. If the writer overwrites an
admitted checkpoint during consumption, the Store withholds that View and switches once to
reading the sealed minutes. A completed pinned window keeps its selection and leaves live
ingestion parked.

Overview waits for the Store instead of asking the legacy page reader to load the
same period. Initial bootstrap, incomplete histories, other pages and unavailable
Store data retain the page-reader path. The Store continues ingesting while other
pages follow the live Mac, so returning to Overview finds its newest View ready. A
hidden window parks it, and so does a held second, whether from Pause, a range dragged on
a chart or a pinned observation, and a saved recording shown in a live window. A cold
restore validates the complete saved predecessor minute, including refused samples, but
constructs no discarded presentation objects for its ticks.

## Measurements

Measured on 2026-10-26, Apple M5 Pro, macOS 27.0.1 (26A434), release builds with six
Polars compute threads and one asynchronous thread. The frozen real recording held
7,882 sealed windows and a 120-publication live ring; the current 1,080 observations
held 498,050 App rows. Both builds read the same immutable files, dictionary bytes,
raw counters, run identities and endpoint, publication 12,671.

These measurements predate holding the loaded hour and keeping the Store active on
other live pages: the builds they measured loaded only the selected period. The
measurements of returning to Overview and switching ranges below cover the current
behavior.

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

A reverse-order repeat with that day's final implementation, seven warm 15-minute restores
per build, measured 21.5 ms and 11.7 ms CPU for optimized, then 207.0 ms and 758.4 ms
CPU for baseline. The final repeat includes the post-admission overwrite recovery and
reopen scheduling corrections. Foreground loss or incomplete initial histories invalidate
a run; the repeat waits for complete initial histories before its discarded warmup.

These warm restores have no new publications. A second experiment starts each fresh
process behind the frozen endpoint, parks ingestion, then reveals 1,800 publications.
This exceeded ring retention, so the measured builds reloaded the selected period.
Checkpoints hide whole sealed minutes, so the initial displayed endpoint is about 30
minutes behind.
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
and replacement overlap.

The files were filesystem-cache warm: measured disk-read bytes were zero. These
numbers establish neither cold-storage performance nor a percentile latency target.
They are local observations on a busy development Mac, not timing thresholds for CI.

### Returning to Overview and switching ranges

Measured on 2026-10-28, Apple M5 Pro, macOS 27.2 (26B123), release builds with the same
pools. Background activity was not controlled. Each native figure is the CPU frame upper
bound described above, from one fresh process. The measured executables have these
SHA-256 identities:

| Executable | SHA-256 |
| --- | --- |
| `flightdeck` | `9a5222b5aba807cabacc8d8012a49691fcb4b7ef9f62bf6e8748ec75bc314bf9` |
| `flightdeck-control` | `1963146e4d1f54f68bf3863e6cd111ec1895695abd6dbaa92f3e9ecc232d49cb` |
| `resume_bench` | `b304d649512af600859112e0fddb8440beefebcc624b3040073a31a1a1dca871` |

The fixture froze the newest 150 sealed windows of the real recording. With the CPU page
showing, the fixture revealed 150 or 3,500 publications at once, and the return to
Overview then drew in 14.9 and 14.8 ms, with 28.4 and 21.6 ms of process CPU. The Store
had kept its View current on CPU, so the return drew that View at once. The navigation
still asks the page reader for the page, as every navigation does, and the charts do not
wait for its reply.

Revealing a gap at once is not what a real stay does. It overruns the ring of the active
Store, which then goes on from the ring's oldest publication and leaves the overwritten
seconds out of its View, so the measured Views lacked most of the gap. A real stay
publishes one second at a time, which the Store ingests as it goes. Two real stays on the
live recording could not be measured: about 15 s and 60 s into them, macOS reported the
window as not visible, likely covered by other windows, so it parked the Store as a
hidden window does and drew no frames to time. A View holds at most the loaded hour
however long the stay, and redrawing a whole hour's View on live Overview took 5.8 to
14.6 ms on the live recording, but a return after a real stay of minutes or an hour was
not observed.

Switching among 5, 15, 30 and 60 minutes on live Overview drew in 5.6 to 14.1 ms before
leaving Overview and 6.5 to 13.9 ms after returning, in both fixture processes. No switch
submitted a page-reader request or changed the Store's version, so each redrew the hour
the Store already held. Measured disk-read bytes were zero, which is weaker evidence,
since the files were filesystem-cache warm.

Ingesting on other pages showed no CPU cost the measurement could resolve. Over 60 s on
the CPU page of the live recording, this revision used 6.99 and 6.94 s of CPU, against
5.15, 6.92, 7.22 and 8.30 s across four runs of the base `8e7e663`, which parked the
Store there. The base's own runs spread over about 3 s, or 5% of one core, so a smaller
cost would not show.

A hidden window parks the Store, and showing it reads the sealed minutes it missed. The
Store-only gated benchmark, three fresh processes per gap, gives these medians in
milliseconds. A gap counts publications, about one a second. A 30-publication gap stays
inside the ring and reads no sealed minute. The base reloaded only the selected period,
so after a gap longer than 15 minutes it read less than this revision with 15 minutes
selected, and kept only the newest 1,043 seconds where this revision keeps the loaded
hour. Past about half an hour, the catch-up costs about what reloading the whole hour
does. The base and this revision were not run interleaved, and a build of this change
from before its last corrections measured 259.1 ms at 3,500, so differences of about 10%
are within what reruns showed:

| Gap (publications) | This revision, wall / CPU | Base at 15 minutes, wall / CPU | Base at 60 minutes, wall / CPU |
| ---: | ---: | ---: | ---: |
| 30 | 23.3 / 22.3 | 23.9 / 23.6 | 22.5 / 22.5 |
| 150 | 89.4 / 180.9 | 137.6 / 489.9 | 209.2 / 789.5 |
| 1,800 | 207.1 / 665.5 | 138.9 / 504.9 | 206.0 / 777.6 |
| 3,500 | 236.1 / 782.7 | 135.4 / 481.7 | 202.7 / 760.7 |

## Profiling and reproduction

`sample` on the original active Store found 337 stack samples under
`recording::terminal`, including 330 under its full payload decoder. That path rebuilt
saved presentations to retain a raw predecessor. The optimized warm native
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
resume_bench --recording FIXTURE --iterations 1 --gated
```

The archived builds above load only the selected period and take `--minutes 5` or
`--minutes 60` as well.

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

To measure a return to Overview from another page, send `checkpoint N` to the fixture
owner, then launch with a `--seconds` deadline longer than the run, since the app quits
when it elapses:

```console
flightdeck --recording FIXTURE --attach-live --control SOCKET --minutes 15 --seconds 600
flightdeck-control --socket SOCKET --command show --command profile-resume
flightdeck-control --socket SOCKET --command 'profile-range 60'
flightdeck-control --socket SOCKET --command 'navigate cpu'
flightdeck-control --socket SOCKET --command 'profile-navigate overview'
```

Send `profile-resume` before the timed commands: `profile-navigate` and `profile-range`
time nothing until it has enabled the measurements, and otherwise fail after 5 s with
"fresh resume frame did not arrive". Wait until `state` reports complete histories and no
pending reply before the first timed command. With the CPU page showing, send `advance`
to the fixture owner, then wait until `state` reports the fixture's endpoint as
`selected_second` and the Store active, unpaused and not pending before
`profile-navigate`. The Store already reads active before the advance, so that state alone
does not show it has read what the fixture revealed. Keep the window uncovered
throughout, since a window macOS reports as not visible parks the Store and draws no
frames to time.

`profile-navigate ROUTE` opens the route as a sidebar click does and returns the first
CPU frame after it that draws the Store's View on live Overview, or that follows the page
reader's next reply on any other page. `profile-range MINUTES` picks the range as the
range picker does and, on live Overview, returns the first frame after it. Use it there
only: with a second held it returns the next frame whatever that frame draws, and on any
other live page it waits for a page-reader reply that a range switch never requests. A
sample whose `worker_requests` is zero and whose `store_version` equals the previous
sample's read nothing for that switch.

For stack profiling, attach `/usr/bin/sample PID 3 1 -mayDie -file OUTPUT` while a
separate control client runs repeated cycles. Use those samples for attribution, not
latency comparisons. The root and desktop checks exercise the frozen fixture, ring
coverage, identity changes, hidden/reopened single-reader behavior, pinned
state, predecessor rates, malformed input, cancellation and presentation bounds.
