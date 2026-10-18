# Prototype review record

The review covered `gpui-prototype/` and its independent job in
`.github/workflows/check.yml`. Four independent Codex reviewers covered spec,
architecture/security, style/refactoring, and testing. The screener assigned high
effort to spec and architecture, medium to style and testing, and low bundled
security/refactoring coverage. All used the parent's Codex model. No axis was
skipped. Reviewers read the project Tiger Style rules and the relevant shared
review standards. They made no edits and launched no applications.

Their initial reports describe the reviewed snapshot before the fixes below.
The author verified fixes; no second code review round was run. A separate review
covers the performance report's claims.

## Findings and dispositions

Spec, blocking, verbatim:

> Captured axis identifiers and units are rendered incorrectly. The catalog uses
> `right1`, `right2`, and `hidden`; the renderer only recognizes `"right"`. At
> sample 120, Open files puts “Share of limit” 4.3159 on the approximately
> 15,910-file scale and labels it `4.3 files`. Correlation treats Fans 1406 rpm,
> Power 31.697 W, and temperature 58.136 °C as percentages.

Fixed: `Line` retains its unit; `axis_domain` groups the captured axis identifiers;
legends use each line's unit and secondary scales are labelled. The real captured
Open files chart is covered by `keeps_secondary_units_out_of_the_primary_scale`.
The fixture check and runtime smoke load and render the correlation panel.

Spec, should-fix, verbatim:

> Several visible ranking links lead to an empty app chart area. At default
> sample 120, click `grafana`, `com.apple.WebKit.WebContent`, or
> `SetStoreUpdateService` in the memory ranking. The snapshot contains their app
> totals, but `catalog.app_pages` contains only the twelve apps with captured
> process histories. Lookup returns None, so no app-total charts or explanatory
> process state render.

Fixed: apps without process captures receive six aggregate histories from their
captured metric series and an explicit unavailable-process-history notice.
`retains_app_totals_when_process_histories_were_not_captured` checks grafana.

Spec, should-fix, verbatim:

> Non-Overview layouts differ materially from the design reference. The renderer
> forces two equal columns for every non-Overview run. v30 CPU begins with three
> panels using 1.35/.85/1.6 proportions; GPUI moves its heatmap to a separate
> full-width row. GPU begins with three charts in v30 but two in GPUI.
> Attribution chart/ranking uses 2:1 in v30 versus 1:1 here.

Fixed the named row groups and proportions: CPU/GPU opening rows use three
columns; CPU's initial weights are 1.35/.85/1.6; chart/ranking pairs use 2:1.
The source and runtime smoke verify the grouping path. Pixel comparison remains
unverified because desktop automation refused the new app. Responsive layout and
the simplified schematic remain prototype limits, not production parity claims.

Style, blocking, verbatim; architecture/security and spec reported the same defect
as should-fix:

> Several clickable legends never hide their series. On CPU, click a core legend
> in `cores-now` or `core-heatmap`; on Overview, click a pressure legend.
> `chart_view.rs` updates hidden state and dims the legend, but `charts.rs` omit[s]
> hidden state when calling timeline, heatmap, and bar painters. Their data
> remains painted.

Fixed: every painter receives the selected panel's hidden series and skips them,
preserving bar/heatmap positions. Hidden selections are keyed per panel. Source
inspection confirms the dispatch and each painter. Pointer-driven visual
verification remains unavailable through desktop automation.

Style, blocking, verbatim; architecture/security reported the same defect as
should-fix:

> Paused fans still drive continuous rendering. Launch `--hardware --animate`,
> then click “Pause animation.” `hardware.rs` changes `Dashboard.animate`, but
> `performance.rs` requests frames from immutable `options.animate`, which stays
> true. The fans stop visually while the entire visible dashboard continues
> rendering, contaminating a subsequent idle observation.

Fixed: the performance driver schedules only explicit scrub/smoke work. Fan
animation requests frames exclusively from its visible canvas using the current
animation state. `--animate` requires the visible Overview hardware workload.
The runtime animation workload verifies the resulting frame loop. Clicking Pause
has not been observed through UI automation; the scheduling path was inspected.

Style, should-fix, verbatim:

> Seven compound conditions violate the explicit repository rule. Sites:
> dashboard.rs:157; performance.rs:45,154; charts.rs:124,220,258. (Six sites;
> performance.rs:45 has three terms.)

Fixed the named compound expressions with explicit branches or pattern matches.
A focused search finds no remaining `&&`/`||` boolean conditions in authored Rust;
the remaining `||` tokens are zero-argument closures. Clippy, formatting, physical
function lengths and column limits were checked.

Testing, should-fix, verbatim:

> The missing-sample test can pass while real gaps become zero readings.
> `tests.rs` counts nulls in raw deserialized fields, then queries an unknown
> metric. Changing `Capture::value` from `.flatten()` to
> `.map(|value| value.unwrap_or(0.0))` leaves both assertions unchanged while
> actual missing samples return `Some(0.0)`. The bundled first `appCpu` series has
> a gap at sample 0, confirmed by a read-only JSON probe.

Fixed: the test now queries that actual gap and checks its unavailable readout.

Testing, should-fix, verbatim:

> The destination/range test verifies counts, not that destinations resolve.
> It still passes if a `pages` or `hour_pages` key such as `"cpu"` becomes
> `"cpu-typo"` while map lengths remain unchanged. `--check` also passes because
> `main.rs` checks navigation membership and prints counts. The UI then cannot
> retrieve that route's panels through `dashboard.rs`'s map lookup.

Fixed: catalog loading validates each navigation destination in both ranges; the
test compares both complete key sets with navigation. All eight tests pass on
the CI-pinned Rust 1.95 toolchain.

## Scope and remaining evidence

The spec review marked Rust/GPUI, isolation and performance-focused structure
present. It marked exact web parity and comparative performance proof partial:
the shared captures and live render loop support a feasibility experiment, while
matched active Swift measurements and visual/input QA remain outstanding.
Production/live feature parity was not requested. No further architecture,
security, refactoring, or CI contract defects were reported.

Build verification uses the independent manifest: formatting, clippy with denied
warnings, eight unit tests, headless capture validation and an optimized runtime
smoke across eleven destinations. `verification.txt` contains the compiler/test
output; runtime and process-observer output are preserved alongside it.

## Performance documentation review

A fresh Codex documentation reviewer recalculated all six observations, checked
the three retained binary hashes and compared claims with the instrumentation.
It reported no blocking finding and one should-fix wording issue, verbatim:

> PERFORMANCE.md overstates the render instrumentation. “The prototype completes
> roughly 120 view renders per second … This establishes that GPUI can run this
> presentation at the display's cadence.” `Dashboard::render` calls
> `Performance::frame` first, and that method increments the counter before the
> remaining view construction, virtual-list rendering and canvas painting. A
> callback can therefore be counted before rendering completes; its interval
> provides no completion or presentation deadline evidence.

Fixed: the opening now describes view-render callback frequency and the CPU-side
loop, with explicit GPU presentation/input limitations. The source inspection
confirms this wording's scope; no runtime rerun is needed for a prose correction.
The reviewer confirmed the report does not claim a comparative active-performance
winner and already discloses the measurement and baseline-provenance gaps.
Proceed with those limits; no second documentation round was run.
