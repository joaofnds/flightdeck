# Evaluation review record

Independent Codex reviews covered the prototype diff, then the keyboard, native
replay shell, measurement helpers and evidence additions. Coverage included spec,
architecture, testing, security, style and refactoring. No code-review axis was
skipped. Reviewers read Tiger Style and the applicable shared review standards;
they made no edits or UI actions. Authors ran the integrated checks afterward.

## Earlier code findings and dispositions

These are finding summaries, rather than verbatim reports.

| Finding | Disposition and evidence |
| --- | --- |
| Selected App B could show retained detail from App A during a pending request. | Fixed: exact kind/byte-key guard at every selected-detail consumer; pending, missing and replacement-selection regressions. |
| Hardware replay could compare static GPUI against animated Swift. | Fixed: hardware plans enable GPUI animation automatically. Runtime eligibility still needs checking; the attempted hardware comparison was rejected. |
| f32 accumulated fan motion loses precision over multi-day sessions. | Fixed: f64 accumulation, modulo before bounded narrowing; multi-day phase regressions. |
| Hashing waited on an external process without a bound. | Fixed: bounded output, deadline and owned-child termination/reaping; stalled-child regression. |
| Rotor tests covered only the zero-angle transform. | Fixed: asymmetric quarter-turn and mirrored-pixel regressions. |
| Timed runs checked shutdown only during renders. | Fixed: retained independent deadline timer; quiet 60-second run exited after three renders. |
| Clickable controls lacked keyboard activation. | Fixed in source: focusable controls, focus styling, Tab traversal and GPUI Enter/Space click support. Interactive verification remains blocked. |
| Rapid arrow presses could derive both requests from the same accepted snapshot. | Fixed: pending inspection request precedes accepted selection; two-step and reversal regressions. |
| LIVE.md still named implemented CPU/subtotal/airflow features as absent. | Corrected current parity statement; historical findings remain historical. |

## Measurement review

Blocking: none established. The two should-fix findings are preserved verbatim:

> **Valid maximum replays exceed the log-reader bounds.**
> `benchmark_report.rs:58,128` permits 10,002 total lines, while Replay accepts
> 10,000 actions and the new instrumentation adds up to 16 chart records and 256
> hardware records, alongside start/summary/window records.
> `benchmark_helpers.rs::trace` similarly stops after 10,010 lines. A valid maximum
> workload therefore either fails reporting or loses its summary during runner
> validation. Fix: share a bound covering actions plus all bounded instrumentation,
> or bound event categories separately and reject actual overflow. Verify with an
> in-memory 10,000-action trace containing instrumentation before its summary.

Disposition: fixed. Shared `LOG_LINES_MAX` covers 10,000 actions plus 1,024 bounded
diagnostic records. Both readers reject actual overflow. The in-memory maximum
trace regression retains all 10,000 actions and its final summary.

> **Acceptance timestamps describe batch entry, excluding action execution.**
> `replay_dashboard.rs::replay_tick` and Swift `NativeReplay.step` sample elapsed
> once before applying up to 64 actions, then reuse that timestamp in records
> emitted after application. The report treats those values as action acceptance
> latency. During catch-up, later actions receive the first action’s timestamp,
> understating their queue delay and excluding execution time. Fix: timestamp each
> completed action and accumulate summaries from completed actions, or consistently
> rename this metric to timer-dispatch lateness. Verify with an injected clock
> advanced during two action applications. This touches earlier replay code; merge
> with any existing disposition rather than recording twice.

Disposition: fixed. Schema 2 records each completed model application and requires
every dispatched action to complete. An advancing-clock regression checks 125 ms
and 150 ms completions after two 25 ms applications. Schema 1 logs retain their
original bytes and are reported only as batch-dispatch timing. The inspection
resource measurements use schema 1; no completion-latency claim uses them.

The same reviewer found no additional resource-math or exploitable security
defect. Evidence notes identify the 932-point-height idle pilot, missing paint
instrumentation in the frozen inspection binaries, and the source-series bound
fix from 32 to 128. None supplies pixel or presentation verification.

## Documentation review

Blocking: none. Should-fix, verbatim:

> **Replay recipe misses a required build.** `gpui-prototype/README.md:109`
> invokes `native_replay_build`, but `src/bin/native_replay_build.rs:31–35`
> requires the root workspace’s `target/release/libviewer.a`. Neither the initial
> GPUI build nor the documented `process_watch` build guarantees that archive
> exists. Add a root prerequisite: `cargo build --release --locked -p viewer --lib`.
> Verified by source reasoning and `rg -n 'crate-type' viewer/Cargo.toml`;
> I did not run builds.

Disposition: fixed. README now gives the root build and distinguishes its working
directory from the prototype commands. The required staticlib declaration and
builder precondition were reread.

Note, verbatim:

> **Retained evidence needs portable navigation.**
> `evidence/evaluation/inspection-summary.json:8–10` points into
> `/Users/joaofnds/.../target/evaluation-inspection/`, although the retained logs
> live beside it under `evidence/evaluation/inspection/`. EVALUATION.md also
> supplies no direct evidence links. Add relative links or an evidence index while
> preserving original provenance fields. The quiet-idle claim at
> `EVALUATION.md:61–65` currently depends on ignored `target/idle-profile-*` files;
> retain those if keeping the claim. Verified using `nl`, directory inventory and
> `.gitignore`.

Disposition: fixed. The report preserves caller-relative input paths, the retained
summary names the evidence tree, and the evaluation links raw records and checks.
Quiet-idle raw observations and app logs are retained under `verification/`.
Original run/build manifests preserve local launch provenance.

The reviewer independently recomputed all six CPU/footprint endpoints and the
table medians, checked 600 equal actions per run, counted 16 populated paint
events (four active), checked quiet-idle arithmetic and rotor byte count, and
verified the v30 reference hash. Sources included all four complete prototype
documents, raw evidence, replay/report/build helpers, paint/font/schematic source,
GLOSSARY.md, Tiger Style, root README, research decisions, CI and ignore rules.

Requirements remain partial for visual/design parity and schematic/CPU interaction.
Native pixels/input, matched live Swift performance, GPU presentation timing and
settled live-memory behavior remain unverified. The review supplied no framework
ranking. Proceeding after the source and arithmetic probes; no second document
review round is planned for these bounded navigation/recipe fixes.

## Native replay follow-up

The runtime hardware attempt exposed two further baseline problems: its scroll
target was nested below a lazy section, and its AppKit shell did not supply the
SwiftUI scene phase. The direct-section scroll target produced a handled-scroll
acknowledgment and positive assembly clipping. Forwarding actual NSApplication
activation through an observed scene phase then produced enabled TimelineView
eligibility while active, and disabled eligibility when offscreen/inactive.
The 30-second probe and its optimized Swift build manifest are retained.

A focused independent measurement-source review confirmed the shared log bound,
per-action completion clocks, legacy timing labels, relative report paths and
activation bridge. It found no synthetic activation forcing. The previous
hardware resource attempt remains excluded; no replacement comparative result
is claimed from the follow-up probe.
The follow-up found no blocking or should-fix issue. Its note required fresh
runtime evidence for the scroll target and activation bridge; the retained probe
provides app-owned geometry and eligibility evidence, with pixels still open.
Live mode enables the same visibility-gated clock by default to match Swift;
the visible control can pause it.
