import AppKit
import Foundation
import SwiftUI

func runSelectedCPUChecks() throws {
    try preserves_selected_logical_cpu_activity()
    rejects_cpu_activity_from_another_observation()
    rejects_a_duplicated_logical_cpu()
    rejects_an_invalid_cpu_percentage()
    rejects_a_nonconsecutive_cpu_interval()
    try preserves_unavailable_first_cpu_sample()
    try accepts_the_maximum_logical_cpu_domain()
    rejects_cpu_percentage(-0.1)
    rejects_cpu_percentage(.nan)
    rejects_cpu_percentage(.infinity)
    rejects_an_incomplete_cpu_domain()
    rejects_an_oversized_cpu_domain()
    rejects_available_cpu_values_without_an_interval()
    rejects_cpu_values_from_another_boot()
    represents_zero_full_and_unavailable_cpu_bars()
    print("Selected logical CPU checks passed")
}

@MainActor
func runSelectedCPUPresentationChecks() throws {
    try reflects_cpu_activity_theme_role(\.cpuActivityLow)
    try reflects_cpu_activity_theme_role(\.cpuActivityModerate)
    try reflects_cpu_activity_theme_role(\.cpuActivityHigh)
    try reflects_cpu_activity_theme_role(\.cpuActivityPeak)
    print("Logical CPU activity rendering reflects all four independent theme roles")
}

@MainActor
private func reflects_cpu_activity_theme_role(_ role: WritableKeyPath<FlightdeckTheme, Color>) throws {
    // Goal: each semantic role controls the rendered CPU bar instead of a fixed palette.
    // Method: compare actual view pixels while replacing exactly one role with white.
    let ordinary = FlightdeckTheme()
    var replacement = ordinary
    replacement[keyPath: role] = .white
    let baseline = try cpuActivityPixels(theme: ordinary)

    let changed = try cpuActivityPixels(theme: replacement)
    let repeated = try cpuActivityPixels(theme: ordinary)
    let unchangedDifference = try cpuPixelDifference(before: baseline, after: repeated)
    let themeDifference = try cpuPixelDifference(before: baseline, after: changed)

    precondition(unchangedDifference <= 2,
        "CPU render changed beyond component rounding")
    precondition(themeDifference > 10,
        "CPU activity ignored theme role \(role)")
}

private func cpuPixelDifference(before: Data, after: Data) throws -> Int {
    guard let first = NSBitmapImageRep(data: before), let second = NSBitmapImageRep(data: after),
          first.pixelsWide == second.pixelsWide, first.pixelsHigh == second.pixelsHigh,
          (1...1000).contains(first.pixelsWide), (1...500).contains(first.pixelsHigh),
          first.bitsPerSample == 8, second.bitsPerSample == 8,
          first.samplesPerPixel == 4, second.samplesPerPixel == 4,
          let firstBytes = first.bitmapData, let secondBytes = second.bitmapData else {
        throw RecordingError(message: "Invalid CPU activity render dimensions or format.")
    }

    var maximum = 0
    for row in 0..<first.pixelsHigh {
        for column in 0..<first.pixelsWide {
            for channel in 0..<4 {
                let old = firstBytes[row * first.bytesPerRow + column * 4 + channel]
                let new = secondBytes[row * second.bytesPerRow + column * 4 + channel]
                maximum = max(maximum, abs(Int(old) - Int(new)))
            }
        }
    }

    return maximum
}

@MainActor
private func cpuActivityPixels(theme: FlightdeckTheme) throws -> Data {
    let selected = cpuObservation(publication: 3)
    let payload = cpuPayload(cpus: [LogicalCPUReading(cpu: 0, busyPercent: 0),
        LogicalCPUReading(cpu: 1, busyPercent: nil), LogicalCPUReading(cpu: 2, busyPercent: 100)])
    let readings = try SelectedCPUReadings(payload: payload, selected: selected)
    let renderer = ImageRenderer(content: LogicalCPUActivity(readings: readings, busy: false)
        .environment(\.flightdeckTheme, theme).foregroundStyle(theme.text)
        .frame(width: 1000).fixedSize(horizontal: false, vertical: true))
    guard let image = renderer.cgImage,
          let pixels = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]) else {
        throw RecordingError(message: "Could not render logical CPU activity.")
    }

    return pixels
}

private func rejects_cpu_activity_from_another_observation() {
    // Goal: a superseded source payload cannot be installed under the selected identity.
    // Method: provide a different publication with otherwise valid logical CPU readings.
    let payload = CPUDetailPayload(observation: cpuObservation(publication: 4),
        previousObservation: cpuObservation(publication: 3), intervalAvailable: true,
        unavailableReason: nil, cpus: [LogicalCPUReading(cpu: 0, busyPercent: 25)])

    rejectsCPUDetail(payload, selected: cpuObservation(publication: 3), error: .observationMismatch)
}

private func rejectsCPUDetail(_ payload: CPUDetailPayload, selected: Observation,
                              error expected: CPUDetailError) {
    do {
        let accepted = try SelectedCPUReadings(payload: payload, selected: selected)
        preconditionFailure("Accepted invalid CPU activity for \(accepted.observation.publication)")
    } catch {
        precondition(error as? CPUDetailError == expected)
    }
}

private func rejects_a_duplicated_logical_cpu() {
    // Goal: duplicated CPU identifiers cannot silently replace a missing logical CPU.
    // Method: provide two rows for CPU zero in a purported two-CPU domain.
    let selected = cpuObservation(publication: 3)
    let payload = CPUDetailPayload(observation: selected,
        previousObservation: cpuObservation(publication: 2), intervalAvailable: true,
        unavailableReason: nil, cpus: [LogicalCPUReading(cpu: 0, busyPercent: 25),
            LogicalCPUReading(cpu: 0, busyPercent: 75)])

    rejectsCPUDetail(payload, selected: selected, error: .domain)
}

private func rejects_an_invalid_cpu_percentage() {
    // Goal: a malformed per-CPU percentage is rejected instead of clamped into a valid bar.
    // Method: provide a finite source value above the one-logical-CPU limit.
    let selected = cpuObservation(publication: 3)
    let payload = CPUDetailPayload(observation: selected,
        previousObservation: cpuObservation(publication: 2), intervalAvailable: true,
        unavailableReason: nil, cpus: [LogicalCPUReading(cpu: 0, busyPercent: 101)])

    rejectsCPUDetail(payload, selected: selected, error: .percentage)
}

private func rejects_a_nonconsecutive_cpu_interval() {
    // Goal: a plausible percentage cannot make a skipped source interval look continuous.
    // Method: claim an available interval using nonconsecutive source publications.
    let selected = cpuObservation(publication: 3)
    let payload = CPUDetailPayload(observation: selected,
        previousObservation: cpuObservation(publication: 1), intervalAvailable: true,
        unavailableReason: nil, cpus: [LogicalCPUReading(cpu: 0, busyPercent: 25)])

    rejectsCPUDetail(payload, selected: selected, error: .interval)
}

private func preserves_unavailable_first_cpu_sample() throws {
    // Goal: a first captured CPU sample keeps explicit unavailable readings without a fake zero.
    // Method: validate two source identifiers with no predecessor or measured interval.
    let payload = cpuPayload(cpus: [LogicalCPUReading(cpu: 0, busyPercent: nil),
        LogicalCPUReading(cpu: 1, busyPercent: nil)], previous: nil, available: false,
        reason: "Previous CPU reading unavailable")

    let readings = try SelectedCPUReadings(payload: payload, selected: payload.observation)

    precondition(!readings.intervalAvailable)
    precondition(readings.previousObservation == nil)
    precondition(readings.cpus == payload.cpus)
}

private func accepts_the_maximum_logical_cpu_domain() throws {
    // Goal: the complete maximum CPU domain and fully busy readings fit without truncation.
    // Method: validate all 1024 sequential identifiers at exactly 100% utilization.
    let cpus = (0..<1024).map { LogicalCPUReading(cpu: UInt32($0), busyPercent: 100) }
    let payload = cpuPayload(cpus: cpus)

    let readings = try SelectedCPUReadings(payload: payload, selected: payload.observation)

    precondition(readings.cpus == cpus)
}

private func rejects_cpu_percentage(_ value: Double) {
    // Goal: nonfinite and negative measurements cannot become valid rendered bar lengths.
    // Method: validate each malformed percentage at an otherwise valid one-CPU observation.
    let payload = cpuPayload(cpus: [LogicalCPUReading(cpu: 0, busyPercent: value)])

    rejectsCPUDetail(payload, selected: payload.observation, error: .percentage)
}

private func rejects_an_incomplete_cpu_domain() {
    // Goal: a source row cannot stand in for a missing lower-indexed CPU.
    // Method: provide only CPU one in a one-row domain.
    let payload = cpuPayload(cpus: [LogicalCPUReading(cpu: 1, busyPercent: 50)])

    rejectsCPUDetail(payload, selected: payload.observation, error: .domain)
}

private func rejects_an_oversized_cpu_domain() {
    // Goal: the decoder rejects a result above its working and rendering bound.
    // Method: provide 1025 individually valid sequential CPU rows.
    let cpus = (0..<1025).map { LogicalCPUReading(cpu: UInt32($0), busyPercent: 50) }
    let payload = cpuPayload(cpus: cpus)

    rejectsCPUDetail(payload, selected: payload.observation, error: .domain)
}

private func rejects_available_cpu_values_without_an_interval() {
    // Goal: an unavailable interval cannot smuggle plausible measured percentages into the UI.
    // Method: provide a nonnil measurement while declaring the interval unavailable.
    let payload = cpuPayload(cpus: [LogicalCPUReading(cpu: 0, busyPercent: 25)],
        previous: nil, available: false)

    rejectsCPUDetail(payload, selected: payload.observation, error: .interval)
}

private func rejects_cpu_values_from_another_boot() {
    // Goal: rates never cross a machine boot even if publication and wall times line up.
    // Method: use a predecessor from another boot at the otherwise adjacent publication.
    let previous = Observation(run: "run", boot: "other", publication: 2, runTick: 2,
        wallNs: 2_000_000_000, windowLosses: nil)
    let payload = cpuPayload(cpus: [LogicalCPUReading(cpu: 0, busyPercent: 25)], previous: previous)

    rejectsCPUDetail(payload, selected: payload.observation, error: .interval)
}

private func cpuPayload(cpus: [LogicalCPUReading], previous: Observation? = cpuObservation(publication: 2),
                        available: Bool = true, reason: String? = nil) -> CPUDetailPayload {
    CPUDetailPayload(observation: cpuObservation(publication: 3), previousObservation: previous,
        intervalAvailable: available, unavailableReason: reason, cpus: cpus)
}

private func represents_zero_full_and_unavailable_cpu_bars() {
    // Goal: segmented bars preserve exact limits while distinguishing unavailable from measured zero.
    // Method: count illuminated segments and inspect numeric presentation for boundary samples.
    precondition((0..<20).filter { CPUActivitySegments.isLit(value: 0, segment: $0) }.isEmpty)
    precondition((0..<20).filter { CPUActivitySegments.isLit(value: 100, segment: $0) }.count == 20)
    precondition((0..<20).filter { CPUActivitySegments.isLit(value: 25, segment: $0) }.count == 5)
    precondition((0..<20).filter { CPUActivitySegments.isLit(value: nil, segment: $0) }.isEmpty)
    precondition(CPUActivitySegments.readout(nil) == "–")
    precondition(CPUActivitySegments.readout(0) == "0%")
}

private func preserves_selected_logical_cpu_activity() throws {
    // Goal: exact source identity and separate measured-zero/unavailable CPU values survive.
    // Method: validate a reordered three-CPU payload from adjacent observations.
    let selected = cpuObservation(publication: 3)
    let payload = CPUDetailPayload(observation: selected,
        previousObservation: cpuObservation(publication: 2), intervalAvailable: true,
        unavailableReason: nil, cpus: [LogicalCPUReading(cpu: 2, busyPercent: nil),
            LogicalCPUReading(cpu: 0, busyPercent: 0), LogicalCPUReading(cpu: 1, busyPercent: 75)])

    let readings = try SelectedCPUReadings(payload: payload, selected: selected)

    precondition(readings.observation == selected)
    precondition(readings.cpus == [LogicalCPUReading(cpu: 0, busyPercent: 0),
        LogicalCPUReading(cpu: 1, busyPercent: 75), LogicalCPUReading(cpu: 2, busyPercent: nil)])
}

private func cpuObservation(publication: UInt64) -> Observation {
    Observation(run: "run", boot: "boot", publication: publication, runTick: publication,
        wallNs: publication * 1_000_000_000, windowLosses: nil)
}
