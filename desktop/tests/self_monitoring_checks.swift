import AppKit
import SwiftUI

@MainActor
func runSelfMonitoringChecks() {
    // Diagnostics distinguish absent evidence, source completion and partial history failure.
    let absent = FlightdeckDiagnostics(catalog: nil, collector: .starting,
        live: true, readerStatus: "No live readings", readerError: nil, busy: true)
    precondition(absent.collectorStatus == "Connecting")
    precondition(absent.historyStatus == "Unavailable")
    precondition(absent.lossStatus == "Unavailable")
    precondition(absent.viewerStatus == "Reading")

    let loss = ReaderLosses(run: "run", boot: "boot",
        losses: Losses(publications: 0, events: 0, unknown: true))
    let partial = Catalog(path: "/recording", firstSecond: 1, lastSecond: 900,
        windows: 14, unknownClockTicks: 0, tables: [], live: true,
        collectorCompleted: true, readerLosses: [loss], historyError: "Seal unavailable")
    let completed = FlightdeckDiagnostics(catalog: partial, collector: .running,
        live: true, readerStatus: "Collector stopped", readerError: nil, busy: false)
    precondition(completed.collectorStatus == "Stopped")
    precondition(completed.historyStatus == "Unavailable")
    precondition(completed.lossStatus == "Loss reported")

    let failed = FlightdeckDiagnostics(catalog: partial, collector: .failed("Producer failed"),
        live: true, readerStatus: "Live", readerError: "Read refused", busy: false)
    precondition(failed.collectorError == "Producer failed")
    precondition(failed.readerReading == "Error")
    precondition(failed.viewerStatus == "Read failed")

    let available = Catalog(path: "/recording", firstSecond: 1, lastSecond: 900,
        windows: 14, unknownClockTicks: 0, tables: [], live: true,
        collectorCompleted: false, readerLosses: [])
    let connected = FlightdeckDiagnostics(catalog: available, collector: .running,
        live: true, readerStatus: "Paused", readerError: nil, busy: false)
    precondition(connected.collectorStatus == "Connected")
    precondition(connected.historyStatus == "14 windows")
    precondition(connected.lossStatus == "None reported")
    precondition(connected.readerReading == "Paused")
    precondition(connected.viewerStatus == "Ready")

    let saved = FlightdeckDiagnostics(catalog: available, collector: .running,
        live: false, readerStatus: "Recorded", readerError: nil, busy: false)
    precondition(saved.collectorStatus == "Saved source")
    precondition(saved.lossStatus == "Unavailable")
    reports_known_and_zero_reader_losses()
    renders_diagnostics_during_reader_failure()
    print("Self-monitoring retains unknown loss, stopped source and independent reader/history failures")
}

private func reports_known_and_zero_reader_losses() {
    // Known publication and event losses are independent; an explicit zero report stays zero.
    for loss in [Losses(publications: 3, events: 0, unknown: false),
                 Losses(publications: 0, events: 7, unknown: false),
                 Losses(publications: 0, events: 0, unknown: false)] {
        let catalog = Catalog(path: "/recording", firstSecond: 1, lastSecond: 900,
            windows: 14, unknownClockTicks: 0, tables: [], live: true,
            readerLosses: [ReaderLosses(run: "run", boot: "boot", losses: loss)])
        let diagnostics = FlightdeckDiagnostics(catalog: catalog, collector: .running,
            live: true, readerStatus: "Live", readerError: nil, busy: false)
        let expected = loss.publications > 0 || loss.events > 0 ? "Loss reported" : "None reported"

        precondition(diagnostics.lossStatus == expected)
    }
}

@MainActor
private func renders_diagnostics_during_reader_failure() {
    // Changing the warning role must repaint diagnostics even when normal metric routing fails.
    // Moving the diagnostics route behind Console's error gate removes this response.
    let model = RecordingModel(path: "/recording", reader: DiagnosticReaderStub(),
        pollingEnabled: false)
    model.startLive()
    model.route = "Self-monitoring"
    model.error = "Reader refused this recording"
    let collector = LiveCollectorSession(process: DiagnosticCollectorStub(), pollingEnabled: false)
    var first = FlightdeckTheme()
    first.warning = .red
    var second = first
    second.warning = .green

    for starting in [false, true] {
        if starting { collector.start() }

        let baseline = diagnosticConsoleImage(model: model, collector: collector, theme: first)
        let recolored = diagnosticConsoleImage(model: model, collector: collector, theme: second)
        let changed = diagnosticPixelChanges(baseline, recolored)

        precondition(changed > 20, "Startup or reader failure hid Self-monitoring diagnostics")
        print("Hosted Console retains diagnostic error content, starting=\(starting): \(changed) pixels")
    }
}

private func diagnosticPixelChanges(_ baseline: NSBitmapImageRep,
                                   _ recolored: NSBitmapImageRep) -> Int {
    precondition(baseline.pixelsWide == recolored.pixelsWide)
    precondition(baseline.pixelsHigh == recolored.pixelsHigh)
    var changed = 0
    for y in 0..<baseline.pixelsHigh {
        for x in 0..<baseline.pixelsWide {
            let source = baseline.colorAt(x: x, y: y)!
            let target = recolored.colorAt(x: x, y: y)!
            if abs(source.redComponent - target.redComponent) > 0.2 { changed += 1 }
        }
    }

    return changed
}

@MainActor
private func diagnosticConsoleImage(model: RecordingModel, collector: LiveCollectorSession,
                                    theme: FlightdeckTheme) -> NSBitmapImageRep {
    let host = NSHostingView(rootView: Console().environmentObject(model)
        .environmentObject(collector).environmentObject(ComputerInventoryModel())
        .environment(\.flightdeckTheme, theme))
    host.frame = NSRect(x: 0, y: 0, width: 1320, height: 900)
    host.layoutSubtreeIfNeeded()
    guard let image = host.bitmapImageRepForCachingDisplay(in: host.bounds) else {
        preconditionFailure("Diagnostic Console did not render")
    }
    host.cacheDisplay(in: host.bounds, to: image)

    return image
}

private final class DiagnosticReaderStub: RecordingReading, @unchecked Sendable {
    func read(_ request: ReadRequest,
              completion: @escaping @Sendable (Result<ReadResult, RecordingError>) -> Void) {
        completion(.failure(RecordingError(message: "Reader refused this recording")))
    }

    func cancel() throws {}
}

@MainActor
private final class DiagnosticCollectorStub: LiveCollectorRunning {
    func start(configuration: LiveCollectorConfiguration,
               event: @escaping @MainActor @Sendable (LiveCollectorEvent) -> Void) {}

    func stop(completion: @escaping @MainActor @Sendable () -> Void) { completion() }
}
