# Live prototype review record

All reviewers used the parent's Codex model and inherited high effort. The screener
assigned dedicated Spec and Architecture reviewers plus bundled Style, Security,
Testing and Refactoring. No axis was skipped. A further scoped reviewer examined
the newly added executable groups/loss notices and their integration. A separate
documentation reviewer read README, LIVE and PERFORMANCE whole.

Code reviewers read Tiger Style, the review-code shared/axis briefs and named house
standards. Main-round coverage: all 21 original changed paths, whole Rust files and
manifest/README, generated lock changes/structure. Additional-round coverage:
live_executables, live_notices, live_dashboard, live_views, live_tables and main.
Reviewers made no edits and ran no suites. Their findings were source-based; parent
runtime checks are distinguished below. The doc reviewer recomputed the retained
JSONL measurements and checked links, reference counts, tests and route/probe logs.

## Findings and dispositions

Quoted findings retain the reviewers' words. Overlapping pagination/default-App
findings from Spec and Architecture are recorded once.

1. **Blocking:** “Computer depends on recording success and initially has no inventory.”
   Fixed: initialize/render inventory immediately and return Computer without opening
   a recording. Observed `computer-missing-recording.json`: one update, null error.

2. **Blocking:** “stale pagination offsets stop live snapshots.”
   Fixed: load page zero, clamp each offset to the current population, retain the
   effective offsets in worker/UI state and reset offsets on inspection. Probe
   reports zero effective offsets for raw, CPU processes, Apps and members after
   requesting enormous aligned offsets.

3. **Blocking:** “chart clicking changes a selected same-second publication.”
   Fixed: retain the current selected identity for that second and otherwise resolve
   a trend observation's exact identity. The selected-second regression test checks
   the chosen publication rather than accepting the first sibling.

4. **Should-fix:** “default selected App changes without selection.”
   Fixed: remember the fallback key in the active worker request and synchronize it
   into UI state. The added regression keeps App A after B becomes first and after
   the loaded page becomes empty.

5. **Should-fix:** “short dashed live runs can disappear.”
   Fixed: geometric pixel dashes replace sample-index omissions; one-point runs draw
   a marker. The two-point geometry test retains a painted segment. Existing null
   and segment separation remains in the painter.

6. **Should-fix:** “measurement recipe now selects the wrong workload.”
   Fixed PERFORMANCE's captured launch with explicit `--reference`; `--check` still
   loads the captured routes successfully.

7. **Should-fix:** “remaining Swift presentation/meaning gaps.”
   Fixed Memory-specific tiles, Normal/Warning/Critical pressure text, CPU process
   fields and pagination, and executable grouping/filtering. Groups intentionally
   expose loaded-page names/counts, with app totals retaining app scope. Full Swift
   group subtotals and the concurrently developed selected per-CPU interval view
   remain partial parity; LIVE/README disclose them. Those are not equivalent
   performance workloads. This prototype uses stable shared APIs and raw CPU data.

8. **Should-fix:** “known collection losses are hidden.”
   Fixed bounded catalog-reader/selected-reader/window notices, retaining exact
   integer counts and unknown state. Tests cover nonzero, unknown and zero losses.

9. **Blocking:** “loss metadata can masquerade as an advancing observation.”
   Fixed probe comparison to exact run/boot/publication. A metadata-only identity
   test and observed publication 405→406 pin the corrected claim.

10. **Should-fix:** “four new compound conditions violate Tiger.”
    Fixed formatting guards to individual range checks. Existing adaptive-unit
    tests and pedantic clippy pass.

11. **Blocking:** “selected historical reader losses can disappear.”
    Fixed deduplication by run/boot, rather than by whether another notice exists.
    The retired-reader regression preserves both current and selected reader losses.

12. **Blocking:** “an executable filter can become invisible and keep hiding processes.”
    Fixed with an always-visible All process groups control and an absent-selected-
    executable notice. Filter/group identity tests cover exact bytes. Mouse recovery
    remains unverified because native computer-use access was refused.

13. **Should-fix:** “listed malformed inputs share one test execution.”
    Split odd-length, non-hexadecimal and oversized executable identities into named
    tests. Each now has its own result.

14. **Notes:** no examined Worker test exercised request supersession; new tests
    lacked AAA phase spacing. Added deterministic request/publication/take
    supersession behavior and a rapid four-request actual-data probe. Added phase
    spacing in the new touched regression/case tests. Reviewers found no concrete
    unsafe/resource/security issue or further structural refactoring need.

15. **Documentation note:** “Unsupported allocation attribution.”
    Removed the unproven cache-filling cause; process samples establish growth only.
    “Polling cadence is approximate”: changed text to 250 ms between read attempts,
    matching the worker sleep after synchronous work. Documentation review had no
    blocking or should-fix findings.

## Requirement accounting

Rust/GPUI, isolation, actual ingestion and performance instrumentation are present.
All nine live destinations use the shared backend or independent host inventory.
Reference v30 remains available unchanged in `--reference`. Prototype parity is
partial in the explicit areas above. The native route runs establish rendering and
real updates, not pixel/input/accessibility fidelity. Measurements establish this
build's observed costs, not a universal or matched Swift speed result. The capacity-
one mailboxes, exact identity checks and single reader owner were retained.

Parent verification: Rust 1.95 fmt, all-target pedantic clippy and the prototype
suite; optimized native runs for nine routes; absent-recording Computer; real-data
probe including pause/resume, missing seconds, offset recovery and overlapping
requests; owned collector start, stop and persister drain. Raw outputs are adjacent.
