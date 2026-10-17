import AppKit
import Foundation
import SwiftUI
import Synchronization

@main
@MainActor
struct NativeReplayApplication {
    static func main() {
        guard CommandLine.arguments.contains("--reference"),
              CommandLine.arguments.contains("--replay") else {
            reportError("Native replay requires --reference --replay ACTIONS.json")
            exit(1)
        }
        let application = NSApplication.shared
        let lifecycle = NativeReplayLifecycle()
        application.delegate = lifecycle
        withExtendedLifetime(lifecycle) { application.run() }
    }
}

@MainActor
private final class NativeReplayLifecycle: NSObject, NSApplicationDelegate {
    private var window: NSWindow?
    private var model: ReferenceModel?
    private let activity = NativeReplayActivity()

    func applicationDidFinishLaunching(_ notification: Notification) {
        if NSApp.activationPolicy() != .regular {
            guard NSApp.setActivationPolicy(.regular) else {
                reportError("Native replay could not become a regular application")
                exit(1)
            }
        }
        let model = ReferenceModel()
        activity.phase = NSApp.isActive ? .active : .inactive
        let content = NativeReplayContent(model: model, activity: activity)
            .frame(minWidth: 840, minHeight: 640)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1320, height: 900),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = "Flightdeck Swift Replay"
        window.contentMinSize = NSSize(width: 840, height: 640)
        window.isReleasedWhenClosed = false
        let hosting = NSHostingView(rootView: content)
        hosting.sizingOptions = []
        window.contentView = hosting
        window.setContentSize(NSSize(width: 1320, height: 900))
        window.center()
        self.model = model
        self.window = window
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        window.contentView?.layoutSubtreeIfNeeded()
        window.setContentSize(NSSize(width: 1320, height: 900))
        NativeReplay.emit(["event": "replay_window", "schema": 1,
            "baseline": "Swift captured views in AppKit replay shell",
            "content_width": window.contentLayoutRect.width,
            "content_height": window.contentLayoutRect.height,
            "scale": window.backingScaleFactor, "visible": window.isVisible,
            "metric": "Constructed application window; not presentation evidence"])
        NativeReplay.install(model)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationDidBecomeActive(_ notification: Notification) {
        activity.phase = .active
    }

    func applicationDidResignActive(_ notification: Notification) {
        activity.phase = .inactive
    }
}

@MainActor
private final class NativeReplayActivity: ObservableObject {
    @Published var phase: ScenePhase = .inactive
}

private struct NativeReplayContent: View {
    let model: ReferenceModel
    @ObservedObject var activity: NativeReplayActivity

    var body: some View {
        ReferenceConsole()
            .environmentObject(model)
            .environment(\.flightdeckTheme, FlightdeckTheme())
            .environment(\.scenePhase, activity.phase)
    }
}

@MainActor
final class NativeReplay {
    static let scrollNotification = Notification.Name("FlightdeckNativeReplayScroll")
    private static var installed: NativeReplay?
    private weak var model: ReferenceModel?
    private let plan: NativeReplayPlan
    private let started = ProcessInfo.processInfo.systemUptime
    private var timer: Timer?
    private var next = 0
    private var latenessMax: UInt64 = 0
    private var latenessTotal: UInt64 = 0
    private var lastCompletedMs: UInt64 = 0
    private static var hardwareStage: CGRect?
    private static var hardwarePanel: CGRect?
    private static var hardwareViewportSize = CGSize.zero
    private static var hardwareAnimates: Bool?
    private static var hardwareLast: NativeHardwareVisibility?
    private static var hardwareLogs = 0
    private static var viewportLogs = 0
    private static var scrollLogs = 0
    private nonisolated static let hardwarePaints = Mutex(UInt64(0))

    nonisolated static func hardwarePaint() {
        hardwarePaints.withLock { count in
            precondition(count < 100_000)
            count += 1
        }
    }

    static func hardwareGeometry(_ frame: CGRect) {
        hardwareStage = frame
        hardwareReport()
    }

    static func hardwarePanelGeometry(_ frame: CGRect) {
        hardwarePanel = frame
        hardwareReport()
    }

    static func hardwareViewport(_ size: CGSize) {
        let changed = size != hardwareViewportSize
        hardwareViewportSize = size
        if viewportLogs == 0 || changed {
            if viewportLogs < 64 {
                viewportLogs += 1
                emitTimed(["event": "replay_viewport_geometry", "schema": 1,
                    "width": size.width, "height": size.height,
                    "metric": "Named ScrollView viewport layout; not presentation"])
            }
        }
        hardwareReport()
    }

    static func scrollDispatched(_ destination: String) {
        guard scrollLogs < 256 else { return }
        scrollLogs += 1
        emitTimed(["event": "replay_scroll_dispatched", "schema": 1,
            "destination": destination,
            "metric": "Notification handled; scrollTo invocation issued, not completed"])
    }

    private static func emitTimed(_ value: [String: Any]) {
        var value = value
        value["wall_ms"] = Date().timeIntervalSince1970 * 1000
        if let installed { value["elapsed_ms"] = installed.elapsedMs() }
        emit(value)
    }

    static func hardwareAnimation(_ enabled: Bool) {
        hardwareAnimates = enabled
        hardwareReport()
    }

    static func hardwareGone() {
        hardwareStage = nil
        hardwarePanel = nil
        hardwareAnimates = nil
        hardwareLast = nil
    }

    private static func hardwareReport() {
        guard let stage = hardwareStage, stage.width > 0, stage.height > 0,
              hardwareViewportSize.width > 0, hardwareViewportSize.height > 0,
              stage.minX.isFinite, stage.minY.isFinite,
              stage.width.isFinite, stage.height.isFinite,
              hardwareViewportSize.width.isFinite, hardwareViewportSize.height.isFinite else { return }
        let state = NativeHardwareVisibility(stage: stage, viewport: hardwareViewportSize,
            panel: hardwarePanel, animation: hardwareAnimates, active: NSApp.isActive)
        guard state != hardwareLast, hardwareLogs < 256 else { return }
        hardwareLast = state
        hardwareLogs += 1
        let clipped = stage.intersection(CGRect(origin: .zero, size: hardwareViewportSize))
        let visible = !clipped.isNull && !clipped.isEmpty
        var value: [String: Any] = ["event": "hardware_visibility", "schema": 1,
            "engine": "Swift", "visible": visible,
            "geometry_scope": "Assembly image inside the labeled schematic",
            "clipped_width": visible ? clipped.width : 0,
            "clipped_height": visible ? clipped.height : 0,
            "stage_width": stage.width, "stage_height": stage.height,
            "viewport_width": hardwareViewportSize.width,
            "viewport_height": hardwareViewportSize.height,
            "window_active": state.active, "animation_enabled": NSNull(),
            "animation_verified": state.animation != nil,
            "metric": "App-owned layout and TimelineView eligibility; not presentation"]
        if let animation = state.animation { value["animation_enabled"] = animation }
        if let panel = state.panel { value["panel_height"] = panel.height }
        emitTimed(value)
    }

    static func install(_ model: ReferenceModel) {
        let arguments = CommandLine.arguments
        guard let index = arguments.firstIndex(of: "--replay") else { return }

        do {
            guard arguments.contains("--reference"), index + 1 < arguments.count else {
                throw NativeReplayError("--replay needs --reference and an action file")
            }
            guard installed == nil else { return }
            guard model.snapshot != nil else {
                throw NativeReplayError("Captured reference did not load")
            }
            let replay = try NativeReplay(model, path: arguments[index + 1])
            installed = replay
            replay.start()
        } catch {
            emit(["event": "replay_error", "message": error.localizedDescription])
            exit(1)
        }
    }

    private init(_ model: ReferenceModel, path: String) throws {
        self.model = model
        plan = try NativeReplayPlan.load(path)
    }

    private func start() {
        let timer = Timer(timeInterval: 1.0 / 30.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.step() }
        }
        self.timer = timer
        RunLoop.main.add(timer, forMode: .common)
        Self.emit(["event": "replay_started", "schema": 2,
            "duration_ms": plan.durationMs, "actions": plan.actions.count,
            "baseline": "Swift captured views in AppKit replay shell",
            "metric": "CPU model application completed; not layout or presentation"])
    }

    private func step() {
        Self.hardwareReport()
        guard let model else {
            Self.emit(["event": "replay_error", "message": "Reference model was released"])
            exit(1)
        }
        let elapsed = elapsedMs()
        for _ in 0..<64 {
            guard next < plan.actions.count else { break }
            let action = plan.actions[next]
            guard action.atMs <= elapsed else { break }

            apply(action, to: model)
            let state = effective(model, action)
            let completed = elapsedMs()
            guard completed >= elapsed, completed >= lastCompletedMs else {
                Self.emit(["event": "replay_error", "schema": 2,
                    "message": "Replay completion clock moved backward"])
                exit(1)
            }
            lastCompletedMs = completed
            let lateness = completed - action.atMs
            latenessMax = max(latenessMax, lateness)
            latenessTotal += lateness
            Self.emit(["event": "replay_action", "schema": 2, "index": next,
                "scheduled_at_ms": action.atMs, "accepted_at_ms": completed,
                "acceptance_lateness_ms": lateness, "effective": state,
                "metric": "CPU model application completed; not layout or presentation"])
            next += 1
        }
        if elapsedMs() >= plan.durationMs, next == plan.actions.count {
            finish()
        }
    }

    private func elapsedMs() -> UInt64 {
        let elapsed = (ProcessInfo.processInfo.systemUptime - started) * 1000
        guard elapsed.isFinite, elapsed >= 0, elapsed <= Double(plan.durationMs + 15_000) else {
            Self.emit(["event": "replay_error", "schema": 2,
                "message": "Replay clock exceeded its finite duration and shutdown grace"])
            exit(1)
        }
        return UInt64(elapsed)
    }

    private func apply(_ action: NativeReplayAction, to model: ReferenceModel) {
        if let minutes = action.minutes {
            model.window.range(lower: minutes == 15 ? 90 : 0, upper: 120)
        }
        if let app = action.app { model.inspect(app: app) }
        if let process = action.process { model.process = process }
        model.route = action.route
        if let sample = action.sample { model.inspect(sample: Int(sample)) }
        if let scroll = action.scroll {
            NotificationCenter.default.post(name: Self.scrollNotification,
                object: model, userInfo: ["scroll": scroll])
        }
    }

    private func effective(_ model: ReferenceModel, _ action: NativeReplayAction) -> [String: Any] {
        var value: [String: Any] = ["route": model.route, "sample": model.window.selected,
            "lower": model.window.lower, "upper": model.window.upper,
            "app": model.app, "process": model.process]
        if let scroll = action.scroll { value["scroll_requested"] = scroll }
        return value
    }

    private func finish() {
        timer?.invalidate()
        timer = nil
        Self.emit(["event": "replay_summary", "schema": 2, "duration_ms": plan.durationMs,
            "actions": plan.actions.count, "accepted": next,
            "last_completed_at_ms": lastCompletedMs,
            "hardware_paint_count": Self.hardwarePaints.withLock { $0 },
            "acceptance_lateness_ms_max": latenessMax,
            "acceptance_lateness_ms_total": latenessTotal,
            "metric": "CPU model application completed; not layout, GPU completion or presentation"])
        Self.installed = nil
        NSApp.terminate(nil)
    }

    fileprivate static func emit(_ value: [String: Any]) {
        do {
            var bytes = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
            bytes.append(10)
            try FileHandle.standardOutput.write(contentsOf: bytes)
        } catch {
            reportError("Native replay output failed: \(error.localizedDescription)")
            exit(1)
        }
    }
}

private struct NativeHardwareVisibility: Equatable {
    let stage: CGRect
    let viewport: CGSize
    let panel: CGRect?
    let animation: Bool?
    let active: Bool
}

private func reportError(_ message: String) {
    do { try FileHandle.standardError.write(contentsOf: Data((message + "\n").utf8)) }
    catch { exit(1) }
}

private struct NativeReplayPlan: Decodable {
    let schema: UInt16
    let durationMs: UInt64
    let actions: [NativeReplayAction]
    private static let byteLimit = 4 * 1024 * 1024
    private enum CodingKeys: String, CodingKey {
        case schema, durationMs = "duration_ms", actions
    }

    static func load(_ path: String) throws -> Self {
        let file = try FileHandle(forReadingFrom: URL(fileURLWithPath: path))
        defer { close(file) }
        let bytes = try file.read(upToCount: byteLimit + 1) ?? Data()
        guard bytes.count <= byteLimit else { throw NativeReplayError("Replay exceeds 4 MiB") }
        try validateKeys(bytes)
        let plan = try JSONDecoder().decode(Self.self, from: bytes)
        try plan.validate()
        return plan
    }

    private static func close(_ file: FileHandle) {
        do { try file.close() }
        catch {
            reportError("Native replay close failed: \(error.localizedDescription)")
        }
    }

    private static func validateKeys(_ bytes: Data) throws {
        guard let object = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              Set(object.keys).isSubset(of: ["schema", "duration_ms", "actions"]),
              let actions = object["actions"] as? [[String: Any]], actions.count <= 10_000 else {
            throw NativeReplayError("Invalid replay document")
        }
        let keys: Set<String> = ["at_ms", "route", "sample", "minutes", "scroll", "app", "process"]
        for action in actions {
            guard Set(action.keys).isSubset(of: keys) else {
                throw NativeReplayError("Unknown replay action field")
            }
        }
    }

    private func validate() throws {
        guard schema == 1, (1...600_000).contains(durationMs) else {
            throw NativeReplayError("Invalid replay schema or duration")
        }
        var previous: UInt64 = 0
        for action in actions {
            try action.validate(durationMs: durationMs)
            guard action.atMs >= previous else {
                throw NativeReplayError("Replay actions are not in scheduled order")
            }
            previous = action.atMs
        }
    }
}

private struct NativeReplayAction: Decodable {
    let atMs: UInt64
    let route: String
    let sample: UInt16?
    let minutes: UInt16?
    let scroll: String?
    let app: String?
    let process: String?
    private enum CodingKeys: String, CodingKey {
        case atMs = "at_ms", route, sample, minutes, scroll, app, process
    }

    func validate(durationMs: UInt64) throws {
        let routes = ["overview", "cpu", "apps", "gpu", "memory", "energy", "storage",
            "network", "battery", "computer", "self"]
        guard atMs <= durationMs, routes.contains(route) else {
            throw NativeReplayError("Invalid replay action time or route")
        }
        if let sample, !(90...120).contains(sample) {
            throw NativeReplayError("Replay sample must be 90...120")
        }
        if let minutes, ![15, 60].contains(minutes) {
            throw NativeReplayError("Replay minutes must be 15 or 60")
        }
        if let scroll {
            guard ["top", "hardware"].contains(scroll) else {
                throw NativeReplayError("Invalid replay scroll target")
            }
            if scroll == "hardware", route != "overview" {
                throw NativeReplayError("Hardware scroll requires Overview")
            }
        }
        for value in [app, process].compactMap({ $0 }) {
            guard !value.isEmpty, value.utf8.count <= 4095, !value.contains("\0") else {
                throw NativeReplayError("Invalid replay selection")
            }
        }
    }
}

private struct NativeReplayError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}
