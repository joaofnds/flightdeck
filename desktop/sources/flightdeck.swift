import AppKit
import SwiftUI

@main
struct Flightdeck: App {
    @StateObject private var recording: RecordingModel

    init() {
        let arguments = CommandLine.arguments
        let path: String
        if let index = arguments.firstIndex(of: "--recording"), arguments.count > index + 1 {
            path = arguments[index + 1]
        } else {
            // Foundation resolves the current user's home through the system user database.
            path = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Application Support/Flightdeck/flightdeck.recording").path
        }
        _recording = StateObject(wrappedValue: RecordingModel(path: path))
    }

    var body: some Scene {
        WindowGroup {
            Console().environmentObject(recording).environment(\.flightdeckTheme, FlightdeckTheme())
                .frame(minWidth: 840, minHeight: 640)
        }.defaultSize(width: 1320, height: 900)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Open recording…", action: recording.open).keyboardShortcut("o")
            }
        }
    }
}
