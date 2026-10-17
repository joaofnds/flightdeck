import AppKit
import SwiftUI

@main
struct Flightdeck: App {
    @NSApplicationDelegateAdaptor(FlightdeckLifecycle.self) private var lifecycle
    @StateObject private var recording: RecordingModel
    @StateObject private var collector: LiveCollectorSession

    init() {
        let arguments = CommandLine.arguments
        let path: String
        if let index = arguments.firstIndex(of: "--recording"), arguments.count > index + 1 {
            path = arguments[index + 1]
        } else {
            path = LiveCollectorConfiguration.defaultRecordingURL.path
        }
        let reference = arguments.contains("--reference")
        let model = RecordingModel(path: path, showsReference: reference)
        let session = LiveCollectorSession(onFailure: {
            if model.isLiveSource { model.cancel() }
        })

        _recording = StateObject(wrappedValue: model)
        _collector = StateObject(wrappedValue: session)
        lifecycle.configure(recording: model, collector: session)

        if !arguments.contains("--recording"), !reference {
            model.startLive()
            session.start()
        }
    }

    var body: some Scene {
        WindowGroup {
            ApplicationConsole().environmentObject(recording).environmentObject(collector)
                .environment(\.flightdeckTheme, FlightdeckTheme())
                .frame(minWidth: 840, minHeight: 640)
        }.defaultSize(width: 1320, height: 900)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Open recording…", action: recording.open).keyboardShortcut("o")
            }
        }
    }
}
