# GPUI implementation and measurements

The isolated GPUI client now includes live and sealed recording workflows,
logical-CPU history, complete selected Memory quantities, pressure and swap
traffic, SoC/fan history, recording context and same-second observation choices.
It packages its own collector and persister and shares Swift's recording format.
The [README](README.md) gives build and launch commands;
[LIVE.md](LIVE.md) records runtime verification and the native acceptance checklist.

The measurements below identify specific implementations and workloads. Historical
captured Overview inspection used less CPU and more memory than the Swift baseline;
different chart layouts and unverified presentation prevent a framework ranking.
No matched visible live comparison or native pixel/input verification is claimed.

## Completion checks

The integrated client checks passed 334 test executions, strict all-target clippy
and formatting under Rust 1.95.0. The root workspace passed 1,206 tests with four
ignored, formatting, strict lint and the Swift `--check` build/runtime fixtures.
Text-backend checks and the captured Swift replay typecheck also passed. The final
client count includes failed-open message preservation, delayed cleanup recovery,
installed Open actions and rendered unavailable/restored controls.

An optimized relocated bundle with SHA-256
`17ac2f5cb6b1ee7e2081aa8c56e783651473dee029bfec7dd6c9e56c36f40cec`
started its own collector/persister, observed advancing registered ring headers,
and saved publications 1 through 12 with reported publication/event loss zero.
The app observed collector exit zero and persister wait status zero. Both helpers
left; disk-only reopening succeeded with producer completion true. An earlier
supervised abrupt-parent test reproduced and corrected a diagnostic-pipe abort.
The final platform-quit observer has a TestPlatform regression; actual Dock Quit
remains an outside native check.

A deterministic projection experiment used the same 900-point input, full panel
output and floating-point output bits over twenty repetitions. It removed 4,502
allocation requests and 123,540 requested bytes per projection on each tested
route. Overview fell from 12,720 to 8,218 requests and from 387,621 to 264,081
requested bytes. This measures Rust allocation requests, not resident memory or
whole-app speed. The optimization is committed as `7f7f1bc`; later source-ready
Memory/Energy features change the whole workload.

Local checkpoint evidence is retained under ignored `target/completion-*`,
`target/lifecycle-final-*` and `target/projection-allocation/`. The original
historical evidence links below remain committed and portable.

## Sustained current CPU history

A 920-second optimized CPU window accepted 926 updates and invoked its root view
render 929 times. Its final snapshot held 900 scalar points and 900 CPU-history
observations for all 18 logical CPUs, with `error:null`. Its selected reader metadata
reported one publication lost and zero events lost, with unknown false. The final
raw cache charge was 132,876,976 bytes for 68 observations, below its 128 MiB budget.
Independent histories retain more observations than that detailed raw cache.

After five seconds of warmup, two sequential process observers covered 903.407
actual seconds. CPU averaged 7.39% of one core. Physical footprint moved from
435.50 to 368.58 MiB, with sampled range 279.52–474.27 MiB; RSS moved from 544.17
to 658.19 MiB. Both observers and the app exited zero. These measures do not
establish a memory plateau or a leak. Source-level cache budgets do not bound
the whole process, which also includes query engines and framework resources.

The measured binary was `17ac2f5c…`, identified in full above, before the final
retirement-notice correction. It was not overwritten during the run. The delivered
binary is `27212e936f578b506da200e0c48ee733945e3af0961e688166ef784913662842`
and has separate final runtime checks: 36 snapshots across all nine routes,
pause/resume and absent-second recovery, source reopening, and a 20-second CPU
window with 19 updates, 22 root renders and 900 observations for all 18 CPUs.
That window's final error was null; its scalar context also held 900 points.
The selected App-history probes retain the reader session's available observations,
not a claim of complete saved history for every App. Logs, source manifests and
the calculation are under `target/completion-cpu-sustained/`; final packaging/checks are under
`target/completion-delivery/`.

Native work, local builds/tests and ordinary host activity continued. The native
owner also reported QA-window activations. Render calls do not establish screen
presentation, and no uninterrupted foreground visibility was observed. This is
an absolute real-data history/resource run, with the separate producer excluded
from app CPU, rather than a Swift comparison or a framework speed result.

## Historical font correction

The first prototype accidentally disabled GPUI 0.2.2's macOS font backend.
`default-features = false` without `font-kit` selects `NoopTextSystem`; its glyph
raster bounds are empty. That explains the blank labels in the reported window.
The old performance measurements exclude text painting and are invalid baselines
for the intended application. They remain in PERFORMANCE.md and LIVE.md as
historical evidence.

Current builds enable `font-kit` and check real system-font advances and glyph
bounds before opening a window. The regression probe failed with equal 9.6-point
advances for W and i before the fix. It passes with W=15.171875 and i=3.640625
points afterward. This verifies the backend configuration and metrics. Pixel
fidelity remains unverified: computer access was rejected again after the user
explicitly approved native-app testing.

## Historical migration pilot

The isolated pilot pins GPUI and its macOS backend to upstream revision
`56934ec0560bce8929025497411de7f5322fb6ca`, including the macOS accessibility adapter.
It adds a native recording picker, bounded time input with composition support,
JSON copy controls, application menus and accessible chart/row controls. Headless
real-recording probes exercise Overview, CPU, selected App and recording switching.
Actual picker, clipboard, IME, VoiceOver and pixel behavior still need interactive
verification; the computer-use tool rejected access despite user authorization.

A confirmed startup cost came from Overview painting the schematic while its first
recording query was loading, then scrolling it out of view when charts arrived.
Deferring that row until the first snapshot avoids initializing the artwork cache.
Two same-binary 90-second control/treatment pairs reduced the 20–60-second footprint
median from 543.791 to 419.174 MiB and from 557.362 to 437.166 MiB. Each pair ended
with equal update counts, scalar points and accounted tail-cache bytes. These
controls used GPUI 0.2.2 to isolate the row change; they do not measure the upstream
port's independent contribution.

A combined upstream/native-control checkpoint ran for five minutes. Its windowed
footprint median during observer seconds 180–240 was 477.885 MiB, versus 213.799 MiB
for the headless worker run. Footprints still rose over the observation period,
ending near 589 and 340 MiB respectively. These separate runs on a shared machine
do not establish allocation ownership, a memory plateau or a framework speed ratio.
The checkpoint preceded review corrections to recording switching and text input;
its frozen binary hash is
`cf8fd7ea50a1139434e9e6e46df67eca1fc97ea4f7b19d49888ee371f6fdd697`.
Raw pilot logs and disposable probes remain under ignored `target/`; they are local
reproduction material, unlike the committed historical evidence linked below.

This checkpoint preceded the current full-build work. Native
interaction/accessibility checks and repeated matched visible-rendering measurements
were not established by it.
The older captured measurements below describe the pre-pilot implementation.

## Historical sustained live retention

A follow-up 900-second diagnostic window accepted 898 updates and 900 scalar
points without an application error. Independent App and CPU history ledgers
increased from 121 to 900 entries, holding 900 in their last two samples. Their
final charged sizes were 32.7 and 1.23 MiB, below separate 64 MiB budgets.
The live staging engine reported approximately 83–87 MiB. These counters exclude
other allocations and cannot establish a whole-process bound or complete history
for every App. The raw 128 MiB tail budget held about 68–69 observations while
scalar context covered 900 seconds.

A fresh unprofiled 900-second window accepted 897 updates and 900 scalar points
without an application error. Its concurrent reader saw 898 distinct publications
with zero reported reader failures; both retained an explicit one-publication loss
count in their selected record. During estimated process ages 780–870 seconds,
physical-footprint medians were 551.081 MiB for the window and
426.885 MiB for the reader. This single shared-machine pair, with other
apps and local build activity, does not isolate rendering cost or establish a
plateau. Visibility was not independently verified. Worker-disposal samples are
excluded. The frozen binary SHA-256 is
`6c9912bd9a3a1b3b11b0826fcb5c062639f9ed655ae4ecd3df46a4faab958fea`.

Allocation-stack probes identify substantial shared query/table and DuckDB costs.
Window diagnostics also show graphics allocations. Their intrusive tools and
shared-machine activity prevent attributing the complete footprint or confirming
a leak. A Game Memory profiler attach failed and supplied no usable attribution.
Renderer atlas/buffer accounting and a controlled complete draw-path pool experiment
were proposed conditional probes. No speculative memory fix was applied.
Native interaction checks and matched visible Swift comparisons
remain open in that historical evidence.
Raw follow-up logs and copied diagnostic source remain private under ignored
`target/continue-*` and `target/growth-ledger-source/`.

## Pre-pilot implementation

Live mode reads the actual recording through `viewer::LiveSession` on a bounded
worker. It includes selected logical-CPU intervals, compact App/process tables,
measured executable subtotals, chart values/axes/local times, exact App-selection
guards, and the shared schematic assets, callouts and illustrative airflow.
Keyboard controls support Tab, Shift-Tab, Enter and Space; focused charts support
arrow-key inspection. Native keyboard behavior still needs an interactive check.

The schematic uses Swift's motion formulas and a 30 Hz clock that suspends when
inactive or outside the painted viewport. Time accumulation uses f64, with bounded
render phases. To preserve the rotor artwork without direct image rotation in
GPUI 0.2.2, this prototype caches 240 orientations, occupying 41,533,440 CPU bytes
plus chassis and GPU storage.
Its first-paint generation time and resulting footprint belong in the comparison.

A subsequent 75-second live CPU-window run accepted 76 backend updates, reached
publication 75 / run tick 74 and reported no application error. A concurrent
backend probe reported 18 logical CPUs with an available selected CPU interval,
468 Apps, stable paused identity, resumed progress and empty missing selection.
The window rendered only four times during that run, so it does not establish
continuous visible updating or live rendering performance.

## Measurement scope

The replay driver schedules model actions independently of rendering. It runs one
measured application at a time, alternates order across three pairs, records
binary hashes and raw process observations, and requires equal effective action
traces. Both builds are optimized. The Swift baseline hosts the committed
captured Swift views in an explicit AppKit window. Its normal SwiftUI WindowGroup
shell is excluded because the direct command-line launch did not construct a
window. The baseline's build manifest identifies its source and resource hashes.

Hardware replay enables GPUI animation automatically. Both apps emit bounded
layout/animation acknowledgments from their actual schematic views. These record
viewport intersection and animation eligibility; they do not establish screen
presentation, compositor occlusion, GPU completion or input latency.

Captured GPUI follows the approved web layout with three-column Overview panels;
the Swift captured views use two columns. Their action traces can match while the
visible chart work differs. Resource results describe these implementations and
must not be presented as an isolated framework speed ranking. The live Overview
uses Swift's two-column arrangement, but an equivalent repeated live comparison
has not yet been established.

The initial five-second-warmup idle pilot includes substantial transient work and
is not a steady-state estimate. A separate sixty-second quiet GPUI launch rendered
three times; an external 45-second observation reported about 0.035 CPU seconds
and 199.7 MiB physical footprint. Its app focus/visible content were not verified,
so it also cannot establish matched idle efficiency.

## Captured Overview inspection

Three alternating pairs each replayed 600 model actions at 10 Hz for 60 seconds.
Process observations cover 30 seconds after a 20-second warmup. Every run exited
successfully and matched the other implementation's effective action trace.
CPU is cumulative process CPU time divided by observed elapsed time, expressed
as a percentage of one core. Footprint is macOS physical footprint at the final
observation, not peak allocated memory.

| Metric, median [minimum–maximum] | GPUI | Swift |
| --- | ---: | ---: |
| CPU, % of one core | 4.25 [0.82–4.60] | 16.69 [15.55–26.17] |
| Final physical footprint, MiB | 300.83 [201.28–302.72] | 202.05 [193.84–202.72] |

These are descriptive ranges over three runs, without confidence intervals.
The machine was an Apple M5 Pro with 18 logical CPUs and 48 GiB RAM, on AC power,
using its built-in 120 Hz display at 2× scale. GPUI logged a 1320×901-point
viewport; Swift logged 1320×900. Other sessions and background activity were
uncontrolled. No thermal warning was reported by the recorded system query.
Window activation/occlusion and the amount of presented work were not verified
over the inspection sample windows; the results include model work regardless of
whether macOS presented a redraw.

The inspection binaries predate the new chart-paint instrumentation. A subsequent
GPUI smoke run recorded positive clipped canvas bounds and populated series,
including active-window paints, and traversed every captured route. That supports
the current paint/data path; it does not retroactively verify the measured
binary's pixels. The inspection timing records use schema 1 and measure batch
dispatch lateness only. Schema 2 now timestamps each action after model
application; neither schema measures input or display latency.

## Schematic comparison status

The first instrumented hardware attempt is excluded from comparative results.
GPUI reported a visible assembly and briefly enabled animation, then became
inactive and suspended its clock; its 60-second run rendered only 38 times.
Swift reported an accepted hardware-scroll request but no schematic geometry or
animation acknowledgment. Its run was terminated before completion. Equal model
actions therefore did not establish equal animated content.

The copied Swift replay views now attach the hardware scroll target to its direct
lazy section and report viewport geometry and scroll dispatch separately. The
normal Swift sources remain untouched by the harness. These hooks must establish
the requested content before a new animation comparison is accepted.
The AppKit shell now forwards actual NSApplication activation into SwiftUI's
scene phase. A subsequent 30-second probe recorded positive schematic clipping
and enabled animation while active, followed by offscreen/inactive suspension.
It validates the new hooks and scene bridge, not repeated animation performance.

## Retained evidence and reproduction

The [inspection summary](evidence/evaluation/inspection-summary.json) names retained
raw process/app logs under [inspection/](evidence/evaluation/inspection/).
The [run manifest](evidence/evaluation/inspection/manifest.json) identifies frozen
binary, workload and watcher hashes; the [Swift build manifest](evidence/evaluation/inspection-swift-build.json)
identifies the archived desktop source. Its existing viewer archive is identified
by bytes only; the source revision of that archive is unknown. Captured replay
does not query the live backend. Local launch paths in original manifests are
provenance, not portable prerequisites.

[Verification logs](evidence/evaluation/verification/) retain the font red/green
probe, captured-route check, populated paint smoke, quiet-idle observations,
live-window/probe output and integrated checks. The [excluded hardware attempt](evidence/evaluation/hardware/)
retains its unsuccessful run as evidence of the comparison gap.
[Review findings and dispositions](evidence/evaluation/review.md) record the
independent code, measurement and document checks. The [README replay recipe](README.md)
gives build/run commands; use a fresh output directory for each run.

## Remaining evidence

Native pixel and input verification is blocked by the computer-use tool rejection.
The user has already authorized it. Complete per-App live-history coverage, GPU presentation
timing, actual input latency and a matched live Swift run remain open. Successful
model replay and unit tests do not close those gaps.
