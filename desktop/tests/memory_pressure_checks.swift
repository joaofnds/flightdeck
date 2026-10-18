import AppKit
import Foundation
import SwiftUI

@MainActor
func runMemoryPressureChecks() throws {
    try keeps_normal_warning_critical_and_unavailable_snapshots_distinct()
    try prefers_the_exact_selected_observation_inside_a_repeated_second()
    try picks_the_latest_exact_observation_with_deterministic_run_boot_and_publication_ties()
    try prefers_wall_time_over_opposing_run_boot_and_publication_order()
    try prefers_run_over_opposing_boot_and_publication_order_at_equal_wall_time()
    try prefers_boot_over_opposing_publication_order_at_equal_wall_time_and_run()
    try compares_adjacent_large_publications_without_losing_integer_precision()
    try summarizes_normal_pressure_as_normal()
    try summarizes_warning_pressure_as_warning()
    try summarizes_critical_pressure_as_critical()
    try summarizes_missing_pressure_as_unavailable()
    try summarizes_unknown_pressure_as_unavailable()
    try clips_the_shared_context_without_holding_state_across_missing_seconds()
    try inspects_individual_alternatives_without_changing_selection()
    try accepts_the_full_observation_bound_and_rejects_the_next_input()
    try rejects_invalid_context_and_duplicate_observation_ids_without_trapping()
    try supports_the_first_wall_clock_second_without_a_negative_domain()
    try paints_normal_zero_differently_from_unavailable_snapshots()
    try paints_normal_with_its_independent_semantic_role()
    try paints_warning_with_its_independent_semantic_role()
    try paints_critical_with_its_independent_semantic_role()
    try keeps_rendering_shared_inspection_local_until_explicit_selection()
    try normalizes_pointer_inspection_to_the_actual_painted_second()
    try clicks_the_exact_selected_owner_of_a_repeated_second()
    try leaves_a_true_gap_unselected_when_clicked()
    try leaves_points_outside_the_rail_unselected()
    try inspects_the_previous_exact_observation_without_selecting()
    try inspects_the_next_exact_alternative_without_selecting()
    try inspects_the_first_observation_without_selecting()
    try inspects_the_last_alternative_without_selecting()
    try confirms_the_exact_locally_inspected_alternative()
    try clears_shared_inspection_without_selecting()
    try refuses_pointer_inspection_and_selection_while_busy()
    try refuses_keyboard_inspection_and_clearing_while_busy()
    try refuses_exact_selection_while_busy()
}

@MainActor
private func normalizes_pointer_inspection_to_the_actual_painted_second() throws {
    // Goal: pointer pixels inspect the actual second painted at that location.
    // Method: use a 1015-point canvas whose inset 900-point rail maps one point per second.
    let fixture = try PressureInteractionFixture()

    fixture.input().inspect(at: CGPoint(x: 800.5, y: 83),
        size: CGSize(width: 1015, height: 190))

    precondition(fixture.inspectedID == fixture.middle.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
    precondition(fixture.selection.selected?.identity == fixture.selected.identity)
}

@MainActor
private func clicks_the_exact_selected_owner_of_a_repeated_second() throws {
    // Goal: clicking the last cell selects its painted exact owner rather than its later alternative.
    // Method: click a repeated second containing adjacent large publication identities.
    let fixture = try PressureInteractionFixture()

    let result = fixture.input().click(at: CGPoint(x: 949.5, y: 83),
        size: CGSize(width: 1015, height: 190))

    precondition(result == .handled)
    precondition(fixture.selections == [RecordedSelectionTarget(second: 1899,
        identity: fixture.selected.identity)])
    precondition(fixture.inspectedID == nil)
    precondition(!fixture.inspecting)
}

@MainActor
private func leaves_a_true_gap_unselected_when_clicked() throws {
    // Goal: clicking an unobserved second cannot select a fabricated nearest observation.
    // Method: click the empty middle of a sparse context through the production interaction adapter.
    let fixture = try PressureInteractionFixture()

    let result = fixture.input().click(at: CGPoint(x: 500.5, y: 83),
        size: CGSize(width: 1015, height: 190))

    precondition(result == .ignored)
    precondition(fixture.selections.isEmpty)
    precondition(fixture.inspectedID == nil)
    precondition(fixture.inspecting)
}

@MainActor
private func leaves_points_outside_the_rail_unselected() throws {
    // Goal: axis and padding clicks cannot select an observation.
    // Method: click above the rail at the horizontal location of an existing observation.
    let fixture = try PressureInteractionFixture()

    let result = fixture.input().click(at: CGPoint(x: 800.5, y: 12),
        size: CGSize(width: 1015, height: 190))

    precondition(result == .ignored)
    precondition(fixture.selections.isEmpty)
    precondition(fixture.inspectedID == nil)
}

@MainActor
private func inspects_the_previous_exact_observation_without_selecting() throws {
    // Goal: Left changes shared local inspection without changing the global selection.
    // Method: step backward from the exact selected observation through the production adapter.
    let fixture = try PressureInteractionFixture()

    let result = fixture.input().step(offset: -1)

    precondition(result == .handled)
    precondition(fixture.inspectedID == fixture.middle.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private func inspects_the_next_exact_alternative_without_selecting() throws {
    // Goal: Right can inspect an individual alternative inside the selected second.
    // Method: step forward from the exact selected owner to its later observation.
    let fixture = try PressureInteractionFixture()

    let result = fixture.input().step(offset: 1)

    precondition(result == .handled)
    precondition(fixture.inspectedID == fixture.alternative.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private func inspects_the_first_observation_without_selecting() throws {
    // Goal: Home inspects the first available observation without selecting it.
    // Method: inspect the first boundary through the production adapter.
    let fixture = try PressureInteractionFixture()

    let result = fixture.input().boundary(first: true)

    precondition(result == .handled)
    precondition(fixture.inspectedID == fixture.first.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private func inspects_the_last_alternative_without_selecting() throws {
    // Goal: End inspects the final actual alternative without selecting it.
    // Method: inspect the final boundary through the production adapter.
    let fixture = try PressureInteractionFixture()

    let result = fixture.input().boundary(first: false)

    precondition(result == .handled)
    precondition(fixture.inspectedID == fixture.alternative.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private func confirms_the_exact_locally_inspected_alternative() throws {
    // Goal: Return emits the exact inspected target and clears local inspection.
    // Method: seed the same shared bindings used by the view with an unselected alternative.
    let fixture = try PressureInteractionFixture()
    fixture.inspectedID = fixture.alternative.id
    fixture.inspecting = true

    let result = fixture.input().select()

    precondition(result == .handled)
    precondition(fixture.selections == [RecordedSelectionTarget(second: 1899,
        identity: fixture.alternative.identity)])
    precondition(fixture.inspectedID == nil)
    precondition(!fixture.inspecting)
}

@MainActor
private func clears_shared_inspection_without_selecting() throws {
    // Goal: Escape restores the selected readout without emitting a selection.
    // Method: clear a shared local inspection through the production adapter.
    let fixture = try PressureInteractionFixture()
    fixture.inspectedID = fixture.middle.id
    fixture.inspecting = true

    let result = fixture.input().clear()

    precondition(result == .handled)
    precondition(fixture.inspectedID == nil)
    precondition(!fixture.inspecting)
    precondition(fixture.input().readoutID == fixture.selected.id)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private func refuses_pointer_inspection_and_selection_while_busy() throws {
    // Goal: a busy view neither changes shared inspection nor emits a clicked target.
    // Method: hover and click a different real observation while the adapter is busy.
    let fixture = try PressureInteractionFixture()
    fixture.inspectedID = fixture.middle.id
    fixture.inspecting = true
    let input = fixture.input(busy: true)

    input.inspect(at: CGPoint(x: 50.5, y: 83), size: CGSize(width: 1015, height: 190))
    let result = input.click(at: CGPoint(x: 949.5, y: 83),
        size: CGSize(width: 1015, height: 190))

    precondition(result == .ignored)
    precondition(fixture.inspectedID == fixture.middle.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private func refuses_keyboard_inspection_and_clearing_while_busy() throws {
    // Goal: busy arrows, boundary navigation and Escape leave shared inspection unchanged.
    // Method: submit those adapter actions against a seeded middle observation.
    let fixture = try PressureInteractionFixture()
    fixture.inspectedID = fixture.middle.id
    fixture.inspecting = true
    let input = fixture.input(busy: true)

    let step = input.step(offset: 1)
    let boundary = input.boundary(first: true)
    let clear = input.clear()

    precondition(step == .ignored)
    precondition(boundary == .ignored)
    precondition(clear == .ignored)
    precondition(fixture.inspectedID == fixture.middle.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private func refuses_exact_selection_while_busy() throws {
    // Goal: a busy Return cannot emit the currently inspected exact target.
    // Method: confirm an alternative with the same callback and bindings as the view.
    let fixture = try PressureInteractionFixture()
    fixture.inspectedID = fixture.alternative.id
    fixture.inspecting = true

    let result = fixture.input(busy: true).select()

    precondition(result == .ignored)
    precondition(fixture.inspectedID == fixture.alternative.id)
    precondition(fixture.inspecting)
    precondition(fixture.selections.isEmpty)
}

@MainActor
private final class PressureInteractionFixture {
    let first: Metric
    let middle: Metric
    let selected: Metric
    let alternative: Metric
    let selection: Selection
    var inspectedID: String?
    var inspecting = false
    var selections = [RecordedSelectionTarget]()

    init() throws {
        first = try pressurePoint(100, wallNs: 1_000_100_000_000, code: 0)
        middle = try pressurePoint(9_007_199_254_740_993, wallNs: 1_750_100_000_000, code: 1)
        selected = try pressurePoint(9_007_199_254_740_994, wallNs: 1_899_100_000_000, code: 0)
        alternative = try pressurePoint(9_007_199_254_740_995,
            wallNs: 1_899_900_000_000, code: 2)
        selection = pressureSelection([first, middle, selected, alternative], selected: selected)
    }

    func input(busy: Bool = false) -> MemoryPressureInteraction {
        guard let timeline = MemoryPressureTimeline(selection: selection) else {
            preconditionFailure("Invalid pressure interaction fixture")
        }

        return MemoryPressureInteraction(timeline: timeline,
            inspectedID: Binding(get: { self.inspectedID }, set: { self.inspectedID = $0 }),
            inspecting: Binding(get: { self.inspecting }, set: { self.inspecting = $0 }),
            busy: busy, onSelect: { target in
                precondition(self.selections.count < 16)
                self.selections.append(target)
            })
    }
}

private func summarizes_normal_pressure_as_normal() throws {
    // Goal: the actual memory summary labels measured zero as Normal.
    // Method: read RecordedMemoryData through a selection carrying stored code zero.
    try pressureSummaryLabel(code: 0, equals: "Normal")
}

private func summarizes_warning_pressure_as_warning() throws {
    // Goal: the actual memory summary preserves the Warning label.
    // Method: read RecordedMemoryData with stored code one.
    try pressureSummaryLabel(code: 1, equals: "Warning")
}

private func summarizes_critical_pressure_as_critical() throws {
    // Goal: the actual memory summary preserves the Critical label.
    // Method: read RecordedMemoryData with stored code two.
    try pressureSummaryLabel(code: 2, equals: "Critical")
}

private func summarizes_missing_pressure_as_unavailable() throws {
    // Goal: a missing summary verdict remains unavailable.
    // Method: read RecordedMemoryData with no pressure code.
    try pressureSummaryLabel(code: nil, equals: "Unavailable")
}

private func summarizes_unknown_pressure_as_unavailable() throws {
    // Goal: an unknown summary verdict remains unavailable.
    // Method: read RecordedMemoryData with a code outside all three stored states.
    try pressureSummaryLabel(code: UInt64.max, equals: "Unavailable")
}

private func pressureSummaryLabel(code: UInt64?, equals expected: String) throws {
    let point = try pressurePoint(1, wallNs: 1_899_100_000_000, code: code)
    let data = RecordedMemoryData(selection: pressureSelection([point]), theme: FlightdeckTheme())

    precondition(data.pressureText == expected)
}

private func prefers_wall_time_over_opposing_run_boot_and_publication_order() throws {
    // Goal: a later actual wall time wins even when every following key orders against it.
    // Method: oppose the wall-time order with run, boot and publication order in one second.
    let later = try pressurePoint(1, wallNs: 1_000_900_000_000,
        code: 2, run: "a", boot: "a")
    let earlier = try pressurePoint(9_007_199_254_740_993, wallNs: 1_000_100_000_000,
        code: 0, run: "z", boot: "z")

    try pressureBucketOwner([later, earlier], equals: later)
}

private func prefers_run_over_opposing_boot_and_publication_order_at_equal_wall_time() throws {
    // Goal: run order wins a wall-time tie independently of boot and publication order.
    // Method: oppose the run key with both remaining tie-break keys.
    let laterRun = try pressurePoint(1, wallNs: 1_000_900_000_000,
        code: 2, run: "z", boot: "a")
    let earlierRun = try pressurePoint(9_007_199_254_740_993, wallNs: 1_000_900_000_000,
        code: 0, run: "a", boot: "z")

    try pressureBucketOwner([laterRun, earlierRun], equals: laterRun)
}

private func prefers_boot_over_opposing_publication_order_at_equal_wall_time_and_run() throws {
    // Goal: boot order wins independently of publication order when wall time and run tie.
    // Method: give the earlier boot the larger exact publication identity.
    let laterBoot = try pressurePoint(1, wallNs: 1_000_900_000_000,
        code: 2, run: "same", boot: "z")
    let earlierBoot = try pressurePoint(9_007_199_254_740_993, wallNs: 1_000_900_000_000,
        code: 0, run: "same", boot: "a")

    try pressureBucketOwner([laterBoot, earlierBoot], equals: laterBoot)
}

private func compares_adjacent_large_publications_without_losing_integer_precision() throws {
    // Goal: the final tie-break key preserves adjacent integers that collapse to one Double.
    // Method: reverse two otherwise identical observations above the exact floating-point range.
    let later = try pressurePoint(9_007_199_254_740_993,
        wallNs: 1_000_900_000_000, code: 2)
    let earlier = try pressurePoint(9_007_199_254_740_992,
        wallNs: 1_000_900_000_000, code: 0)

    try pressureBucketOwner([later, earlier], equals: later)
}

private func pressureBucketOwner(_ contenders: [Metric], equals expected: Metric) throws {
    let selected = try pressurePoint(100, wallNs: 1_899_100_000_000,
        code: 0, run: "selected", boot: "selected")
    let timeline = MemoryPressureTimeline(selection:
        pressureSelection(contenders + [selected], selected: selected))

    let painted = timeline?.point(at: 0.5 / 900)
    let target = timeline?.target(id: painted?.id)

    precondition(target == RecordedSelectionTarget(second: 1000, identity: expected.identity),
        "The pressure bucket chose the wrong exact observation")
}

@MainActor
private func paints_normal_zero_differently_from_unavailable_snapshots() throws {
    // Goal: observed Normal paints a rail while missing state codes leave it blank.
    // Method: compare only interior rail pixels across 900 sustained observed seconds.
    let normal = try pressureSustainedSelection(code: 0)
    let unavailable = try pressureSustainedSelection(code: nil)

    let differences = pressurePlotPixelDifferences(pressureImage(normal), pressureImage(unavailable))

    precondition(differences > 14_000, "Normal observations did not paint the pressure rail")
}

@MainActor
private func paints_normal_with_its_independent_semantic_role() throws {
    // Goal: Normal uses its semantic role rather than a fixed interval color.
    // Method: recolor only that role while all 900 observed cells are Normal.
    try pressureRolePixelGuard(code: 0, role: \.memoryPressureNormal, name: "Normal")
}

@MainActor
private func paints_warning_with_its_independent_semantic_role() throws {
    // Goal: Warning uses its own semantic role independently of Normal and Critical.
    // Method: recolor only that role while all 900 observed cells are Warning.
    try pressureRolePixelGuard(code: 1, role: \.memoryPressureWarning, name: "Warning")
}

@MainActor
private func paints_critical_with_its_independent_semantic_role() throws {
    // Goal: Critical uses its own semantic role independently of Normal and Warning.
    // Method: recolor only that role while all 900 observed cells are Critical.
    try pressureRolePixelGuard(code: 2, role: \.memoryPressureCritical, name: "Critical")
}

@MainActor
private func keeps_rendering_shared_inspection_local_until_explicit_selection() throws {
    // Goal: displaying a shared local inspection neither selects nor resets the observation.
    // Method: render an unselected alternative through mutable shared inspection bindings.
    let selected = try pressurePoint(1, wallNs: 1_899_100_000_000, code: 0)
    let local = try pressurePoint(2, wallNs: 1_899_900_000_000, code: 1)
    let selection = pressureSelection([selected, local], selected: selected)
    var id: String? = local.id
    var inspecting = true

    let image = pressureImage(selection,
        inspectedID: Binding(get: { id }, set: { id = $0 }),
        inspecting: Binding(get: { inspecting }, set: { inspecting = $0 }))

    precondition(image.pixelsWide == 1080)
    precondition(id == local.id)
    precondition(inspecting)
    precondition(selection.selected?.identity == selected.identity)
}

@MainActor
private func pressureRolePixelGuard(code: UInt64,
                                   role: WritableKeyPath<FlightdeckTheme, Color>,
                                   name: String) throws {
    let selection = try pressureSustainedSelection(code: code)
    var changed = FlightdeckTheme()
    changed[keyPath: role] = Color(red: 0.12, green: 0.32, blue: 0.94)

    let differences = pressurePlotPixelDifferences(pressureImage(selection),
        pressureImage(selection, theme: changed))

    precondition(differences > 14_000, "The \(name) semantic role did not color observed states")
}

@MainActor
private func pressureImage(_ selection: Selection, theme: FlightdeckTheme = FlightdeckTheme(),
                           inspectedID: Binding<String?> = .constant(nil),
                           inspecting: Binding<Bool> = .constant(false)) -> NSBitmapImageRep {
    let renderer = ImageRenderer(content: RecordedMemoryPressure(selection: selection,
        inspectedID: inspectedID, inspecting: inspecting, busy: false, onSelect: { _ in
            preconditionFailure("Rendering changed the selected observation")
        }).frame(width: 1080, height: 340, alignment: .top)
            .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
            .background(theme.background).preferredColorScheme(.dark))
    renderer.scale = 1
    guard let image = renderer.cgImage else { preconditionFailure("The timeline did not render") }

    return NSBitmapImageRep(cgImage: image)
}

private func pressurePlotPixelDifferences(_ first: NSBitmapImageRep,
                                         _ second: NSBitmapImageRep) -> Int {
    precondition(first.pixelsWide == 1080)
    precondition(first.pixelsHigh == 340)
    precondition(second.pixelsWide == first.pixelsWide)
    precondition(second.pixelsHigh == first.pixelsHigh)
    var differences = 0

    // The rail interior excludes heading, axis, selection readout and all legend swatches.
    for y in 124..<142 {
        for x in 90..<970 {
            guard let source = first.colorAt(x: x, y: y)?.usingColorSpace(.sRGB),
                  let target = second.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else {
                preconditionFailure("The rendered rail has no RGB pixel")
            }

            let distance = abs(source.redComponent - target.redComponent)
                + abs(source.greenComponent - target.greenComponent)
                + abs(source.blueComponent - target.blueComponent)

            if distance > 0.06 { differences += 1 }
        }
    }

    return differences
}

private func pressureSustainedSelection(code: UInt64?) throws -> Selection {
    let points = try (0..<900).map { index in
        try pressurePoint(UInt64(index) + 1,
            wallNs: (1_000 + UInt64(index)) * 1_000_000_000 + 100_000_000, code: code)
    }

    return pressureSelection(points)
}

private func prefers_the_exact_selected_observation_inside_a_repeated_second() throws {
    // Goal: the painted second and its pointer target prefer the exact selected alternative.
    // Method: select an earlier Normal observation before a later Critical observation.
    let selected = try pressurePoint(9_007_199_254_740_993,
        wallNs: 1_899_100_000_000, code: 0)
    let later = try pressurePoint(9_007_199_254_740_994,
        wallNs: 1_899_900_000_000, code: 2)
    let selection = pressureSelection([later, selected], selected: selected)

    let timeline = MemoryPressureTimeline(selection: selection)
    let painted = timeline?.point(at: 899.5 / 900)

    precondition(painted?.id == selected.id)
    precondition(timeline?.state(id: painted?.id) == .normal)
    precondition(timeline?.target(id: painted?.id) == RecordedSelectionTarget(second: 1899,
        identity: selected.identity))
}

private func picks_the_latest_exact_observation_with_deterministic_run_boot_and_publication_ties()
    throws {
    // Goal: a second without the selected observation has one deterministic actual owner.
    // Method: shuffle equal wall times across runs, boots and exact 64-bit publications.
    let earlier = try pressurePoint(10, wallNs: 1_000_100_000_000, code: 0)
    let run = try pressurePoint(11, wallNs: 1_000_900_000_000, code: 0, run: "z")
    let boot = try pressurePoint(12, wallNs: 1_000_900_000_000,
        code: 1, run: "z", boot: "z")
    let latest = try pressurePoint(9_007_199_254_740_993, wallNs: 1_000_900_000_000,
        code: 2, run: "z", boot: "z")
    let selected = try pressurePoint(20, wallNs: 1_899_100_000_000, code: 0)
    let selection = pressureSelection([latest, selected, earlier, boot, run], selected: selected)

    let timeline = MemoryPressureTimeline(selection: selection)
    let painted = timeline?.point(at: 0.5 / 900)

    precondition(painted?.id == latest.id)
    precondition(timeline?.state(id: painted?.id) == .critical)
    precondition(timeline?.target(id: painted?.id)?.identity.publication == 9_007_199_254_740_993)
}

private func clips_the_shared_context_without_holding_state_across_missing_seconds() throws {
    // Goal: independent second buckets neither extend a run nor fill a collection gap.
    // Method: put irregular observations and different segments between out-of-domain points.
    let before = try pressurePoint(1, wallNs: 999_900_000_000, code: 2)
    let first = try pressurePoint(2, wallNs: 1_000_200_000_000, code: 0)
    let next = try pressurePoint(3, wallNs: 1_002_900_000_000,
        code: 1, run: "other", boot: "other", segment: 7)
    let selected = try pressurePoint(4, wallNs: 1_899_100_000_000, code: 2)
    let after = try pressurePoint(5, wallNs: 1_900_000_000_000, code: 0)

    let timeline = MemoryPressureTimeline(selection:
        pressureSelection([after, first, before, selected, next], selected: selected))

    precondition(timeline?.firstSecond == 1000)
    precondition(timeline?.endSecond == 1900)
    precondition(timeline?.cells.count == 900)
    precondition(timeline?.observations.map(\.id) == [first.id, next.id, selected.id])
    precondition(timeline?.point(at: 0.5 / 900)?.id == first.id)
    precondition(timeline?.point(at: 1.5 / 900) == nil)
    precondition(timeline?.point(at: 3.5 / 900) == nil)
    precondition(timeline?.point(at: 1) == nil)
    precondition(timeline?.point(at: -.infinity) == nil)
    precondition(timeline?.point(at: .nan) == nil)
}

private func inspects_individual_alternatives_without_changing_selection() throws {
    // Goal: shared local inspection can reach exact alternatives before explicit selection.
    // Method: step between two observations in one second and compare their immutable targets.
    let selected = try pressurePoint(9_007_199_254_740_993,
        wallNs: 1_899_100_000_000, code: 0)
    let alternative = try pressurePoint(9_007_199_254_740_994,
        wallNs: 1_899_900_000_000, code: 1)
    let selection = pressureSelection([alternative, selected], selected: selected)
    let timeline = MemoryPressureTimeline(selection: selection)

    let inspected = timeline?.step(id: selected.id, offset: 1)

    precondition(inspected?.id == alternative.id)
    precondition(timeline?.step(id: selected.id, offset: -1)?.id == selected.id)
    precondition(timeline?.step(id: alternative.id, offset: 1)?.id == alternative.id)
    precondition(timeline?.boundary(first: true)?.id == selected.id)
    precondition(timeline?.boundary(first: false)?.id == alternative.id)
    precondition(timeline?.target(id: inspected?.id) == RecordedSelectionTarget(second: 1899,
        identity: alternative.identity))
    precondition(selection.selected?.identity == selected.identity)
    precondition(timeline?.fraction(id: selected.id) == (899.1 / 900))
}

private func accepts_the_full_observation_bound_and_rejects_the_next_input() throws {
    // Goal: 4096 observations fit inside at most 900 independent display cells.
    // Method: compare dense unique input at the exact bound and one observation beyond it.
    let full = try pressureDensePoints(count: 4096)
    let excessive = try pressureDensePoints(count: 4097)

    let accepted = MemoryPressureTimeline(selection: pressureSelection(full))
    let rejected = MemoryPressureTimeline(selection: pressureSelection(excessive))

    precondition(accepted?.observations.count == 4096)
    precondition(accepted?.cells.count == 900)
    precondition(rejected == nil)
}

private func rejects_invalid_context_and_duplicate_observation_ids_without_trapping() throws {
    // Goal: invalid external context becomes an unavailable timeline rather than an assertion.
    // Method: supply an unsupported domain, duplicate identity and overflowing wall-time domain.
    let point = try pressurePoint(1, wallNs: 1_899_100_000_000, code: 0)

    precondition(MemoryPressureTimeline(selection: pressureSelection([point], context: 901)) == nil)
    precondition(MemoryPressureTimeline(selection: pressureSelection([point, point])) == nil)
    precondition(MemoryPressureTimeline(selection:
        pressureSelection([point], second: UInt64.max)) == nil)
}

private func supports_the_first_wall_clock_second_without_a_negative_domain() throws {
    // Goal: the initial partial context has one real cell rather than an underflowed window.
    // Method: select a Normal observation inside Unix second zero.
    let point = try pressurePoint(1, wallNs: 100_000_000, code: 0)

    let timeline = MemoryPressureTimeline(selection: pressureSelection([point], second: 0))

    precondition(timeline?.firstSecond == 0)
    precondition(timeline?.endSecond == 1)
    precondition(timeline?.cells.count == 1)
    precondition(timeline?.point(at: 0.5)?.id == point.id)
    precondition(timeline?.state(id: point.id) == .normal)
}

private func pressureDensePoints(count: Int) throws -> [Metric] {
    var points = try (0..<(count - 1)).map { index in
        try pressurePoint(9_007_199_254_740_993 + UInt64(index),
            wallNs: 1_000_100_000_000 + UInt64(index) * 219_000_000, code: UInt64(index % 3))
    }
    points.append(try pressurePoint(9_007_199_254_740_993 + UInt64(count - 1),
        wallNs: 1_899_100_000_000, code: 0))

    return points
}

private func keeps_normal_warning_critical_and_unavailable_snapshots_distinct() throws {
    // Goal: measured Normal remains a state while missing and unknown codes stay unavailable.
    // Method: build independent observed seconds and leave a true unobserved gap after them.
    let points = try [
        pressurePoint(10, wallNs: 1_000_100_000_000, code: 0),
        pressurePoint(11, wallNs: 1_001_200_000_000, code: 1),
        pressurePoint(12, wallNs: 1_002_900_000_000, code: 2),
        pressurePoint(13, wallNs: 1_003_000_000_000, code: nil),
        pressurePoint(14, wallNs: 1_004_000_000_000, code: UInt64.max),
        pressurePoint(15, wallNs: 1_899_100_000_000, code: 0)]
    let selection = pressureSelection(points)

    let timeline = MemoryPressureTimeline(selection: selection)

    precondition(timeline?.state(id: points[0].id) == .normal)
    precondition(timeline?.state(id: points[1].id) == .warning)
    precondition(timeline?.state(id: points[2].id) == .critical)
    precondition(timeline?.state(id: points[3].id) == nil)
    precondition(timeline?.state(id: points[4].id) == nil)
    precondition(MemoryPressureState.normal.label == "Normal")
    precondition(timeline?.point(at: 5.5 / 900) == nil)
    precondition(timeline?.point(at: 4.5 / 900)?.id == points[4].id)
}

private func pressurePoint(_ publication: UInt64, wallNs: UInt64, code: UInt64?,
                           run: String = "memory-run", boot: String = "memory-boot",
                           segment: UInt64 = 1) throws -> Metric {
    var value: [String: Any] = ["run": run, "boot": boot, "publication": publication,
        "run_tick": publication, "wall_ns": wallNs, "segment": segment]
    value["pressure"] = code
    let data = try JSONSerialization.data(withJSONObject: ["value": value])

    return try decodeRecordingResponse(data)
}

private func pressureSelection(_ points: [Metric], selected: Metric? = nil,
                               second: UInt64 = 1899, context: UInt64 = 900) -> Selection {
    let chosen = selected ?? points.last
    let observation = chosen.map {
        Observation(run: $0.run, boot: $0.boot, publication: $0.publication,
            runTick: $0.publication, wallNs: $0.wallNs, windowLosses: nil)
    }

    return Selection(second: second, selected: observation,
        alternatives: observation.map { [$0] } ?? [], summary: chosen,
        trends: points, contextSeconds: context)
}
