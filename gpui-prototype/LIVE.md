# Live client verification

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

The delivered app SHA-256 is
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

1. From the repository root, run `open 'gpui-prototype/target/Flightdeck GPUI.app'`.
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
6. With an isolated owned recording, test the app's Quit command, last-window close
   and Dock Quit. Confirm final save and helper exit. Repeat while attaching to an
   independently running collector and confirm that producer continues.

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
