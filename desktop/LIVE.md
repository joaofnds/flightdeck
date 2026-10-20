# Live client verification

Each verification section identifies its own binary and workload. Historical
sections below identify pre-promotion builds.
Current build and launch commands are in [README.md](README.md).

## Current app

The client opens live and sealed recordings through the shared reader. It supports
exact observation selection, paged raw/process/App data, logical-CPU histories,
Memory quantities and pressure, Energy/SoC/fan histories, GPU, Network, Storage,
Battery, Computer and Self-monitoring.
The [README](README.md) gives the self-contained bundle build and launch commands.

Current checks use real encoded sealed recordings and GPUI's in-process TestPlatform.
The latter dispatches rendered chart input, installed actions and Cmd-O, including
pending/failed selection recovery. It does not exercise native macOS event delivery
or inspect screen pixels. Saved-recording fixtures distinguish final same-second
publications and preserve an explicitly chosen earlier observation.

The integrated disk, Battery and route-ranking increments pass 621 canonical desktop
test executions: 521 application, 83 projection benchmark and 17 public CLI.
Rust 1.98.0 normal-profile
fmt, strict all-target Clippy and 30 tidy checks pass on unchanged input manifests.
The transitive block0.1.6 dependency emits a future-compiler incompatibility notice;
Inbox TASK-47 retains that finding without suppression.

Storage attribution scans a real sealed 600-App fixture and retains the exact
same-second publication and leader beyond the first 512-row page. Installed chart
input checks cover separate Read/Write coverage, small rates, historical loading,
failure and restored readings. Four vertical-wheel regressions preserve horizontal
column positions while the surrounding list scrolls. These are TestPlatform and
reader observations, without native event or pixel verification.

The Battery schematic preserves selected charge and signed power, including measured
zero and unavailable values. Installed-input checks cover pending, failed and missing
historical observations, restored live readings, malformed Battery input alongside
valid unrelated readings, and the numeric byte/finite bounds. CPU, Memory and Energy
show their corresponding Top apps card. Real sealed 600-App cases preserve the
leader beyond page one and the exact same-second observation on all three routes.
Historical Memory failure retains its scalar with an explicit App error; missing
Energy clears both. Rendered click and focused Enter activation retain the historical
App identity and reset stale offsets. Enter follows pointer focus in that case;
keyboard-only navigation and native event delivery remain unverified.

The latest optimized package, built from committed source through `6fba081`, passes
captured-route, font, package-resource and app/helper byte checks. Its app SHA-256 is
`fe9de696738ccc2aa1b0537e1a7a58205bd16ca9bd653af41761d9a71085644d`.
A modified artwork copy fails validation. This package reopened the newly collected
recording read-only in a twenty-second window, ending with 900 scalar points and
`error:null`; the saved capture independently contains 900 points. No helper was
started by that reopening. The application reported populated canvases, but those
reports do not establish visible pixels. Build inputs, package identities and raw
reopening output are under ignored `desktop/target/final-candidate-logs/` from the
repository root. The canonical bundle contains those same verified bytes.

The preceding disk-only optimized build passes captured-route and font checks, packaged app/helper
byte identity, resource validation and a tampered-resource refusal. Its executable
SHA-256 is `9c486f20b7979702655d4e4a1819290cad38ce49d045897932bb8a1cbebb42f1`.
A read-only capture from a frozen real recording contains 900 WindowManager App
history points, each with CPU, resident memory, footprint, power and disk readings.
All five production chart projections are present. The cache accounts for 32,816
bytes with neither recorded nor live retention limitation. This capture establishes
saved-data support; it does not establish a currently advancing collector.

The separate `projection_bench` executable has SHA-256
`dfaa8e66a559f1908ab89e991e6a4988dcf3c2876a3e35c8f7648712857a36d3`.
The benchmark and capture-input hashes were recorded before and verified after a
fresh twenty-iteration measurement. Its output transcript matches the earlier
capture's five-panel transcript byte-for-byte. After one warmup, median wall time
is 712,854ns and process CPU time is 713,895.5ns.
Each records 3,684 allocation requests and 146,602 requested
bytes. Counters cover panel preparation only; requested bytes are not retained
memory. Timings include clock/counter overhead and exclude query, decoding,
encoding and UI. Shared-host activity was uncontrolled. This scoped observation
does not establish visible responsiveness, a whole-app resource budget or a
framework ranking. Raw canonical checks and source manifests remain under ignored
`target/disk-attribution/canonical-1.98-repaired/` in the desktop workspace;
optimized package and capture outputs are under the repository root's
`target/polars_migration/desktop_candidate_logs/`. The separately identified
measurement is in the desktop workspace's ignored
`target/disk-attribution/app-measurement-identified/`. The earlier run reported
650,521ns wall and 651,459ns process CPU; its benchmark hash was first observed
after that run, so it has weaker executable provenance.

The earlier ranking increment passed 489 desktop test executions: 442 application,
36 projection benchmark and 11 replay tests. Strict all-target Clippy, formatting
and tidy pass. Production TestPlatform nodes check Overview's 3/2/1
columns at the 1100/640-point boundaries, header control bounds, retained selection
and coherent stationary-pointer inspection through regrouping. Overview now includes
the existing pressure timeline, preserving gaps and the selected same-second publication.

Overview ranks CPU, resident memory and process power over all captured Apps, with
exact integer ordering, proportional bars, metric help and selected-observation
drilldown. Tests cover leaders after the first 512-row page, measured zero, partial
and unavailable measurements, overflow, incomplete membership and deterministic ties.
Installed-input checks preserve the full run/boot/publication and App key through
hover, loading, failure, click and keyboard activation. The scan has a 24,000-App,
47-page bound and retains at most ten entries per metric.

Three production hover lookups over 900 synthetic observations reduced allocation
requests from 10,800 to 12 for the earliest target, preserving the returned indexes.
Requested bytes fell from 345,600 to 384. An absent target fell from 10,800 requests
to zero. These counters exclude fixture setup, rendering and application latency.
The packaged increment's executable SHA-256 is
`ff871c2451fc7cd8d5caf19c08c1543445fe3c739230e456e02d1da3905d4eb6`.
Local commands, raw measurements and independent reviews are retained under ignored
`target/rankings-*`, `target/resume-*` and `target/hover-*`. Its source-input manifests
were identical before and after the final release build. The app and packaged helper
bytes matched their release inputs and bundle validation passed. This artifact uses
the backend before the separately ongoing Polars migration; these results do not
validate that migration. Native acceptance below remains open.
The package and its checks used Rust 1.95.0; current build instructions use the
migration's Rust 1.98.0 prerequisite.

That package's read-only worker probe accepted 39 snapshots across all twelve routes
in 50.953 seconds while the isolated collector advanced. Its initial Overview read
took 3.479 seconds and ranked 476 Apps, with three ten-entry lists and explicit
incomplete membership. Exact pause, advancing resume, missing-reading recovery,
pagination and recording reopen passed. Scalar context and the two selected bundle
and executable histories each covered 900 unique seconds without gaps. Reader
metadata retained two missed publications and zero missed events, unknown false.
These are reader/model checks, not native picker, clipboard, IME or VoiceOver checks.

The earlier responsive-chart package, SHA-256
`7bf4bd16fee30afc16a71c5674b6329667b9cc96ac977bf8e76f2fad0fae5aa4`,
first observed 617 of 900 seconds across a source stop/restart. Its subsequent
300-second read-only soak accepted 300 updates, 296 distinct delivered observations
and no reader failures. It ended with 900 unique history seconds and no context gaps;
the reported missed publication remained recoverable in history. The source had
already been running before that observation, so the 300 seconds alone did not
collect a complete fifteen-minute interval.

After installing the earlier `ff871c24…` ranking package, its owned collector restarted on the same
recording. A 15-second read-only soak accepted 13 distinct advancing updates without
errors. Its history showed 858 of 900 seconds, retaining the 42-second collection
gap from the restart. A later full context must age that gap out or preserve it;
the pre-restart complete-history result does not apply to this window.

An earlier ranking binary, SHA-256
`91933cb25fff88af59bcab3538a0afb1bf3837ef4d958d06bc5d2c1d60096fdf`,
ran a warm headless worker observation over 30.189 seconds at 5.606% of one core.
Physical footprint moved from 278,234,936 to 297,387,880 bytes, with sampled maximum
305,235,816 bytes. That binary predates the final bar/help and parsing changes.
The run excludes a window and rendering; it establishes neither current whole-app
cost, a memory plateau, an improvement nor a framework ranking.

## Advancing Polars recording and cost

The `9c486f20…` package collected a new isolated recording for 1,100 seconds. Its
final app report contains 1,099 accepted updates, 900 scalar points, zero reported
publication/event losses and `error:null`. A separate twenty-second worker soak
observed twenty distinct advancing publications and 900 unique seconds without
internal or context gaps. The final save reports zero losses, and the owned app,
collector and persister were absent after the normal timer expired. LaunchServices
does not provide their Unix exit statuses. This binary predates the Battery and
CPU/Memory/Energy ranking increments above.

An external forty-five-second observation after the history reached 900 points
used 7.1583 CPU seconds over 45.2914 elapsed seconds, or 15.8051% of one core.
The first 42.2728 seconds averaged 9.7506%; a final 3.0186-second interval averaged
100.59%. Footprint moved from 462,047,248 to 472,631,312 bytes. An earlier,
short-history loaded-host sample averaged 3.66%; it is not a matched comparison.
Our and the migration thread's builds were paused for the full-history sample,
but ordinary host activity and sustained window presentation were uncontrolled.

A later five-second stack sample found the main thread mostly waiting and some
live-worker time in scalar JSON cloning/encoding, polling and App ranking responses.
It did not overlap the saturation burst and cannot identify its cause. Repeated
full-context construction and commit-time history restoration remain hypotheses.
No speed, memory-plateau or whole-app performance acceptance follows from these
observations.
Raw app, soak, process-counter and stack outputs are retained under ignored
`desktop/target/disk-attribution/live-acceptance-20261019-0139/`.

## Pre-promotion verification

The Rust 1.95.0 integrated check passed 431 test executions, formatting and strict
all-target clippy. Resource tests exercise historical summaries, device membership,
missing observations, same-second battery states, Copy and restored live values
through production GPUI nodes. Source tests exercise accepted, failed and cancelled
Open actions, Recorded labels, one-response recorded requests, paused diagnostic
updates, return to live following after Open and attached-producer stop/stall recovery.

A packaged real-data probe read all twelve routes in 50.26 seconds, accepting 39
snapshots. Pause held its exact observation, resume advanced, missing selections
remained empty and reopening preserved readable data. Scalar context and the two
selected App histories each covered 900 observed seconds. These selected histories
do not establish complete history for every App. Reader metadata explicitly
reported one missed publication and zero missed events, with unknown false.
The probe attached read-only to the existing Swift-owned collector; it started or
stopped no producer. Its app SHA-256 was
`525dc924ccb4a7b794dc1a7463007ab8d05ea2506be51ddfa0e95e9857925d4c`.
Local source hashes, test logs, packaged executable identities and the raw probe
are under ignored `target/rest-application/`.

That delivered app's SHA-256 was
`03517e429a3b0c8fbb1a2e136c7cfaa4d83140f3dc98c3d1d013ae19f733cd57`.
It separates shared timeline bucketing from pressure-specific metadata. Exact
state, selected-publication, gap and complete help tests pass before and after
that refactor; its full 431-test suite, strict lint/formatting and tidy pass.
The bundle's executable/helper bytes match the release inputs and resources validate.
The preceding probe and resource-window observations identify their own binary.

On the probe binary, Battery and Storage each ran for twenty seconds with 900
scalar history points and no application error. They accepted 18 and 19 updates,
respectively; their canvas callbacks reported populated source points and positive
clipped bounds in an active 1320 × 900 point window at scale 2. These are app-owned
paint and data observations, without independent pixel or native input verification.

The app build includes the native thread's frozen viewer candidate. Its input
manifest identifies those bytes independently of Git HEAD; this is a local app
verification, without a claim that the whole shared workspace passed its release gates.

An actual relocated bundle started its own collector and persister, read all nine
routes, stopped them and reopened their sealed output. An abrupt-parent-departure
probe reproduced a persister diagnostic-pipe abort. Private unlinked append files
removed that failure: the supervised retest observed both helpers leave and reopened
the final recording with producer completion true. Exit statuses after reparenting
were unavailable. A separate in-process regression exercises the final termination
observer; actual Dock Quit remains in the checklist below.

Local raw artifacts are under ignored `target/completion-*` and
`target/parent-departure-*`. They identify the exact binary and source snapshots;
they are reproduction material on the development Mac, not portable CI fixtures.
The historical observations below retain their original binary limitations.

## Native acceptance checklist

Computer access rejected the Flightdeck GPUI app after user authorization. These
steps require an outside observation; no shell screenshot, accessibility automation
or alternate input tool substitutes for that denied access.

1. From the repository root, run `open 'desktop/target/Flightdeck.app'`.
   The default source is `~/Library/Application Support/Flightdeck/flightdeck.recording`.
   Check readable labels, all twelve destinations and populated charts at the normal
   and a narrower window size. Inspect CPU history, all Memory rows, pressure, fan
   gaps, Storage devices, Battery details and unavailable sources against the selected observation.
2. Use File → Open Recording and Cmd-O. Cancel once, open a sealed recording, then
   try an invalid folder. Confirm cancellation/failure preserves the old recording.
3. Hover a plot on Overview, Memory, CPU, Apps, Storage and Battery. Check that the
   cursor stays within its plot and the header, headline, legends, quantities,
   device membership, details, schematic and Copy agree on the hovered observation.
   Move directly to another plot, then into its title, legend, gutters and outside
   the window; the cursor must disappear outside plots. Scroll or resize while
   hovering and inspect an absent second: unrelated live readings must not replace
   missing historical values. Click a chart, inspect a same-second alternative, use
   repeated arrows during loading, then Resume live and confirm current values return.
4. Enter an exact Unix second and a local time. Exercise IME composition, selection
   and replacement. Copy a reading, raw row and App/process detail; inspect the
   system clipboard for exact identities, counter strings and nulls.
5. Use Tab/Shift-Tab and VoiceOver through navigation, time input, charts, CPU rows
   and observation choices. Check labels, focus and actions without pointer input.
6. With an isolated owned recording, close the last window and confirm the Dock icon
   leaves, the menu bar item stays and the collector keeps advancing. Click the item
   and confirm the window returns over the readings collected while it was shut, then
   close it again and use Open Flightdeck from the item's right-click menu. Test Quit
   from that menu, the app's Quit command and Dock Quit. Confirm final save and helper
   exit. Repeat while attaching to an independently running collector and confirm that
   producer continues.

Successful TestPlatform dispatch and command-line lifecycle probes establish their
own paths only. They do not close this native event, accessibility or pixel checklist.

**Historical run limitation:** the 2026-10-17 binary used GPUI's no-op text backend.
Its backend observations below remain useful, but successful render callbacks did
not establish a readable interface. Its UI performance figures exclude text
painting and cannot support a comparison with Swift. Current builds enable the
macOS font backend and verify real system glyph metrics at startup.

The GPUI prototype reads the same `viewer::LiveSession` API as Swift. The original
captured web reference remains behind `--reference`. Live mode has separate models
and never fills unavailable readings from that reference.

The worker owns the recording reader, waits 250 ms between read attempts, and publishes through
capacity-one mailboxes. Generation checks and cancellation reject superseded work.
Paused inspection holds an exact run, boot and publication. Charts retain actual
wall-clock spacing, null gaps and run/boot/segment boundaries within a 900-second,
4096-point context. Raw and App pages hold at most 512 rows.

## Observed on 2026-10-17

Hardware: Mac17,8, Apple M5 Pro, 18 logical CPUs, 48 GiB RAM, macOS 27.0.1 build
26A434. GPUI reported a 1320 × 901 point viewport at scale 2. The optimized binary
was built with local Rust 1.99.1; checks used CI's Rust 1.95.0. Backend compilation
used the shared working tree while another session was adding CPU details. This
is not an isolated revision comparison against Swift.

An actual collector ran in an isolated ring and recording under `target/`. The
[live probe](evidence/live/probe.json) read all nine routes, saw 644 App groups,
validated raw/App/detail/history observation identities, projected the actual
chart payloads, and checked these transitions:

- Pause held publication 405; resume reached 406.
- A future second returned no selected observation, summary or raw payload.
- Stale offsets for raw, CPU processes, Apps and members recovered to zero.
- Four overlapping requests delivered the final request generation.

The probe completed in about 7.3 seconds. Its first recording read took about
1.9 seconds on the worker. This is startup reader work, not UI frame time.

Each of the nine native GPUI windows ran for four seconds. The JSON files beside
the probe record successful updates and `error:null` for each route. Computer also
ran against a nonexistent recording with one inventory update and no error.
These are runtime render checks. Pixel fidelity, mouse/keyboard flows and native
accessibility were not verified: computer-use access was refused in this session.

The [owned collector run](evidence/live/owned-collector-output.log) collected eight
ticks, delivered eight UI updates, and then stopped when the app exited. Its
[shutdown output](evidence/live/owned-collector-errors.log) shows the persister
sealed its window and finished; the collector output records `persister_status=0`.
No process from that run remained in the subsequent process inspection.

All 48 prototype tests passed, including exact identities, nulls, units, time
positions, geometry dashes, mailbox supersession, fan schemas, executable grouping
and loss notices. Rustfmt and all-target pedantic clippy passed. `--check` still
loaded all eleven captured routes, 95 metric families and 121 samples per hour.

## Live resource observation

The bundled Overview ran with an owned collector for 55 seconds, receiving 55
updates and invoking its root view render 57 times, with no reported error. After
a 20-second warmup, the external `process_watch` observed 15.13 seconds:

| Process observation | GPUI live Overview |
|---|---:|
| CPU, percent of one core | 2.23% |
| Physical footprint, first to last | 449.4 → 503.6 MiB |
| Last resident size | 325.6 MiB |
| Idle wakeups per second | 41.1 |
| Interrupt wakeups per second | 129.0 |

The [raw observations](evidence/live/process-watch-20s-warmup.jsonl),
[application output](evidence/live/measured-app.log) and
[binary hashes](evidence/live/binary-hashes.txt) identify this run. Footprint was
still growing during this short run, so 503.6 MiB is an observed endpoint,
not settled memory use. CPU excludes the separately running collector and
persister. Other sessions and collectors were active; window activation was
requested, but visibility was not independently inspected. This is a feasibility
observation, not a controlled comparison to Swift or a frame/input-latency result.

A second process, with a 55-second warmup, used 2.81% of one core during a
15.13-second observation. Physical footprint rose from 659.4 to 676.1 MiB, and
resident size ended at 499.1 MiB. Its [raw samples](evidence/live/process-watch-55s-warmup.jsonl)
still show growth, so neither run establishes settled memory use. Memory retention
needs a longer profile before interpreting the prototype's steady-state cost.

## Historical parity and limits

Live mode implements Overview, CPU & processes, Apps, Memory, GPU, Energy, Network,
Computer and Recorded readings. Memory categories stack only on Overview; dedicated
memory charts keep them independent. Energy series remain independent. Network
sends plot below zero. The schematic uses recorded CPU/GPU activity, installed RAM
and validated fan channels, with unavailable sensors labeled explicitly.

App totals and histories stay at App scope. Executable groups use exact captured
bytes and show measured subtotals and instance filtering for the loaded process
page. CPU activity uses the selected observation's validated logical-CPU interval.
The schematic uses the shared rotor artwork, callouts and illustrative airflow at
30 Hz. Native accessibility and exact chart smoothing remain incomplete.

## Historical review dispositions

Independent Codex reviews covered Spec, Architecture, Style, Security, Testing and
Refactoring; no axes were skipped. Reviewers read Tiger Style and their axis
standards. The main round covered the 21 original changed files; a focused review
covered executable groups, loss notices and their UI integration. Reviewers ran
no suites. The parent ran the checks and runtime probes recorded here.
The [review record](evidence/live/review.md) retains findings and dispositions.

Confirmed and fixed: recording-dependent Computer rendering; invalid retained
offsets; discarded same-second identity; unstable default App; invisible short
dashed runs; the captured measurement recipe's missing `--reference`; unreadable
pressure states and missing Memory tiles; CPU process paging; hidden collection
losses; false advancement from loss metadata; compound formatting guards; hidden
executable filters; and malformed executable inputs combined in one test. Added
regression checks exercise the affected pure/worker behaviors and live pagination.
That historical round covered counts/filtering; its later code also implemented
measured subgroup subtotals and selected logical-CPU intervals. Those additions
needed fresh runtime and performance evidence at that checkpoint.

No security defect or additional structural refactoring was identified. The
after-task pass retained the independent live/captured paths and single reader
owner rather than restructuring the shared backend.

Live resource observations belong beside these checks. They demonstrate the cost
of this build and workload; a framework speed verdict still requires matched
Swift/GPUI history lengths, selected pages, animation, visibility and input, with
identified source revisions. The earlier captured observations are in
[PERFORMANCE.md](PERFORMANCE.md).
