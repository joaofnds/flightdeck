import AppKit
import Foundation
import Darwin

@MainActor
func runLiveCollectorChecks() throws {
    shares_one_collector_across_repeated_startup()
    waits_for_two_advancing_live_publications()
    rejects_a_completed_collector_as_startup_readiness()
    restarts_the_readiness_baseline_when_the_collector_run_changes()
    requires_retry_after_the_startup_deadline()
    exposes_an_owned_collector_failure_after_readiness()
    retains_an_external_producer_after_the_candidate_exits()
    cancels_failed_startup_without_any_view_observer()
    refreshes_producer_ownership_after_a_running_source_changes()
    cancels_polling_after_an_owned_producer_exits_without_any_view()
    ignores_old_attempt_callbacks_after_retry()
    drains_both_real_output_streams_with_fixed_tail_limits()
    try kills_and_reaps_only_its_unresponsive_owned_child()
    try reaps_an_exited_child_before_shutdown_escalation()
    try leaves_an_external_child_running_when_it_owns_none()
    print("15 native live collector checks passed")
}

func runLiveCollectorChild(arguments: [String]) throws -> Bool {
    if arguments.count == 2, arguments[1] == "--live-collector-output-fixture" {
        for index in 0..<256 {
            let standard = Data(repeating: UInt8(index), count: 4096)
            let diagnostic = Data(repeating: UInt8(255 - index), count: 4096)

            try FileHandle.standardOutput.write(contentsOf: standard)
            try FileHandle.standardError.write(contentsOf: diagnostic)
        }
        return true
    }
    if arguments.count == 3, arguments[1] == "--live-collector-held-fixture" {
        signal(SIGTERM, SIG_IGN)
        alarm(5)
        try Data().write(to: URL(fileURLWithPath: arguments[2]))
        pause()
        return true
    }
    if arguments.count == 3, arguments[1] == "--live-collector-exit-fixture" {
        signal(SIGTERM, SIG_IGN)
        alarm(5)
        try Data().write(to: URL(fileURLWithPath: arguments[2]))
        usleep(10_000)
        return true
    }

    return false
}

@MainActor
private func shares_one_collector_across_repeated_startup() {
    // Repeated window startup must share one session and one collector launch.
    let process = ControlledLiveCollector()
    let session = LiveCollectorSession(process: process, pollingEnabled: false)

    session.start()
    session.start()

    precondition(process.starts == 1)
    precondition(session.state == .starting)
}

@MainActor
private func waits_for_two_advancing_live_publications() {
    // An existing saved timestamp and repeated publication cannot establish live readiness.
    let session = LiveCollectorSession(process: ControlledLiveCollector(), pollingEnabled: false)
    session.start()
    let first = RecordedObservationIdentity(run: "run", boot: "boot", publication: 4)
    let second = RecordedObservationIdentity(run: "run", boot: "boot", publication: 5)

    session.observe(identity: first, collectorPID: 42, collectorCompleted: false)
    session.observe(identity: first, collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .starting)

    session.observe(identity: second, collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .running)
}

@MainActor
private func rejects_a_completed_collector_as_startup_readiness() {
    // A stopped collector must not satisfy readiness even if its final publication advances.
    let session = LiveCollectorSession(process: ControlledLiveCollector(), pollingEnabled: false)
    session.start()
    session.observe(identity: .init(run: "run", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)

    session.observe(identity: .init(run: "run", boot: "boot", publication: 5),
        collectorPID: 42, collectorCompleted: true)
    session.observe(identity: .init(run: "run", boot: "boot", publication: 6),
        collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .starting)
}

@MainActor
private func restarts_the_readiness_baseline_when_the_collector_run_changes() {
    // A run boundary requires a second observation from the new run.
    let session = LiveCollectorSession(process: ControlledLiveCollector(), pollingEnabled: false)
    session.start()
    session.observe(identity: .init(run: "old", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)

    session.observe(identity: .init(run: "new", boot: "boot", publication: 5),
        collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .starting)

    session.observe(identity: .init(run: "new", boot: "boot", publication: 6),
        collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .running)
}

@MainActor
private func requires_retry_after_the_startup_deadline() {
    // A queued observation after the deadline cannot revive an expired attempt.
    var instant = 0.0
    let session = LiveCollectorSession(process: ControlledLiveCollector(), pollingEnabled: false,
        now: { instant })
    session.start()
    session.observe(identity: .init(run: "run", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)
    instant = 20

    session.observe(identity: .init(run: "run", boot: "boot", publication: 5),
        collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .failed("Live readings did not advance within 20 seconds."))

    session.retry()
    session.observe(identity: .init(run: "run", boot: "boot", publication: 6),
        collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .starting)

    session.observe(identity: .init(run: "run", boot: "boot", publication: 7),
        collectorPID: 42, collectorCompleted: false)

    precondition(session.state == .running)
}

@MainActor
private func exposes_an_owned_collector_failure_after_readiness() {
    // An owned collector crash after readiness must expose Retry rather than stale live state.
    let process = ControlledLiveCollector()
    let session = LiveCollectorSession(process: process, pollingEnabled: false)
    session.start()
    session.observe(identity: .init(run: "run", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)
    session.observe(identity: .init(run: "run", boot: "boot", publication: 5),
        collectorPID: 42, collectorCompleted: false)

    process.fail("Collector failed")

    precondition(session.state == .failed("Collector failed"))
}

@MainActor
private func retains_an_external_producer_after_the_candidate_exits() {
    // A late canonical-lock loser must not invalidate the external producer already witnessed.
    let process = ControlledLiveCollector()
    let session = LiveCollectorSession(process: process, pollingEnabled: false)
    session.start()
    session.observe(identity: .init(run: "external", boot: "boot", publication: 4),
        collectorPID: 99, collectorCompleted: false)
    session.observe(identity: .init(run: "external", boot: "boot", publication: 5),
        collectorPID: 99, collectorCompleted: false)

    process.exit(status: 1)

    precondition(session.state == .running)
}

@MainActor
private func cancels_failed_startup_without_any_view_observer() {
    // The owner cancels startup polling even when no console or reference view exists.
    var instant = 0.0
    var cancellations = 0
    let session = LiveCollectorSession(process: ControlledLiveCollector(), pollingEnabled: false,
        now: { instant }, onFailure: { cancellations += 1 })
    session.start()
    instant = 20

    session.step()
    session.step()
    session.observe(identity: .init(run: "run", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)

    precondition(cancellations == 1)
    precondition(session.state == .failed("Live readings did not advance within 20 seconds."))
}

@MainActor
private func refreshes_producer_ownership_after_a_running_source_changes() {
    // An external successor run must cease being attributed to the original owned child.
    let process = ControlledLiveCollector()
    let session = LiveCollectorSession(process: process, pollingEnabled: false)
    session.start()
    session.observe(identity: .init(run: "owned", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)
    session.observe(identity: .init(run: "owned", boot: "boot", publication: 5),
        collectorPID: 42, collectorCompleted: false)

    session.observe(identity: .init(run: "external", boot: "boot", publication: 1),
        collectorPID: 99, collectorCompleted: false)
    process.exit(status: 1)

    precondition(session.state == .running)
}

@MainActor
private func cancels_polling_after_an_owned_producer_exits_without_any_view() {
    // A current owned producer exit uses the same view-independent failure callback as startup.
    let process = ControlledLiveCollector()
    var cancellations = 0
    let session = LiveCollectorSession(process: process, pollingEnabled: false,
        onFailure: { cancellations += 1 })
    session.start()
    session.observe(identity: .init(run: "owned", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)
    session.observe(identity: .init(run: "owned", boot: "boot", publication: 5),
        collectorPID: 42, collectorCompleted: false)

    process.exit(status: 9)
    process.exit(status: 9)

    precondition(cancellations == 1)
    precondition(session.state == .failed("Collector exited (9). "))
}

@MainActor
private func ignores_old_attempt_callbacks_after_retry() {
    // A delayed main-queue event from a retired startup attempt must not fail its replacement.
    let process = ControlledLiveCollector()
    var instant = 0.0
    let session = LiveCollectorSession(process: process, pollingEnabled: false, now: { instant })
    session.start()
    let retired = process.callback!
    instant = 20
    session.step()
    session.retry()
    session.observe(identity: .init(run: "owned", boot: "boot", publication: 4),
        collectorPID: 42, collectorCompleted: false)
    session.observe(identity: .init(run: "owned", boot: "boot", publication: 5),
        collectorPID: 42, collectorCompleted: false)

    retired(.failed("Retired attempt"))

    precondition(session.state == .running)
}

@MainActor
private func drains_both_real_output_streams_with_fixed_tail_limits() {
    // Real pipe output greater than kernel capacity must drain without retaining the entire log.
    let process = LiveCollectorProcess()
    let result = CollectorExitCheck()
    let configuration = LiveCollectorConfiguration(
        executable: URL(fileURLWithPath: CommandLine.arguments[0]),
        arguments: ["--live-collector-output-fixture"])

    process.start(configuration: configuration) { result.receive($0) }
    waitForCollectorCheck { result.exit != nil }

    let (status, output) = result.exit!
    let standard = Data(repeating: 252, count: 4096) + Data(repeating: 253, count: 4096)
        + Data(repeating: 254, count: 4096) + Data(repeating: 255, count: 4096)
    let diagnostic = Data(repeating: 3, count: 4096) + Data(repeating: 2, count: 4096)
        + Data(repeating: 1, count: 4096) + Data(repeating: 0, count: 4096)

    precondition(status == 0)
    precondition(output.standardBytes == 1_048_576)
    precondition(output.errorBytes == 1_048_576)
    precondition(output.standard == standard)
    precondition(output.error == diagnostic)
}

@MainActor
private func kills_and_reaps_only_its_unresponsive_owned_child() throws {
    // A real owned child that ignores SIGTERM must exit through the bounded kill deadline.
    let ready = FileManager.default.temporaryDirectory
        .appendingPathComponent("flightdeck-collector-ready-\(UUID().uuidString)")
    defer {
        do { try FileManager.default.removeItem(at: ready) }
        catch { preconditionFailure("Could not remove collector fixture: \(error)") }
    }
    let process = LiveCollectorProcess(shutdownGrace: 0.05)
    let result = CollectorExitCheck()
    let configuration = LiveCollectorConfiguration(
        executable: URL(fileURLWithPath: CommandLine.arguments[0]),
        arguments: ["--live-collector-held-fixture", ready.path])
    process.start(configuration: configuration) { result.receive($0) }
    waitForCollectorCheck { FileManager.default.fileExists(atPath: ready.path) }
    var finished = false

    process.stop { finished = true }
    waitForCollectorCheck { finished && result.exit != nil }

    precondition(result.exit?.0 == SIGKILL)
}

@MainActor
private func reaps_an_exited_child_before_shutdown_escalation() throws {
    // An exit before the kill deadline must be reaped without changing its exit status to SIGKILL.
    let ready = FileManager.default.temporaryDirectory
        .appendingPathComponent("flightdeck-collector-exit-\(UUID().uuidString)")
    defer {
        do { try FileManager.default.removeItem(at: ready) }
        catch { preconditionFailure("Could not remove collector fixture: \(error)") }
    }
    let process = LiveCollectorProcess(shutdownGrace: 0.20)
    let result = CollectorExitCheck()
    let configuration = LiveCollectorConfiguration(
        executable: URL(fileURLWithPath: CommandLine.arguments[0]),
        arguments: ["--live-collector-exit-fixture", ready.path])
    process.start(configuration: configuration) { result.receive($0) }
    waitForCollectorCheck { FileManager.default.fileExists(atPath: ready.path) }
    var finished = false

    process.stop { finished = true }
    waitForCollectorCheck { finished && result.exit != nil }

    precondition(result.exit?.0 == 0)
}

@MainActor
private func leaves_an_external_child_running_when_it_owns_none() throws {
    // Stopping an unowned session must not signal another collector's process.
    let external = Process()
    external.executableURL = URL(fileURLWithPath: "/bin/sleep")
    external.arguments = ["5"]
    try external.run()
    defer {
        if external.isRunning { external.terminate() }
        external.waitUntilExit()
    }
    let process = LiveCollectorProcess()
    var finished = false

    process.stop { finished = true }
    waitForCollectorCheck { finished }

    precondition(external.isRunning)
}

@MainActor
private func waitForCollectorCheck(_ condition: () -> Bool) {
    let deadline = ProcessInfo.processInfo.systemUptime + 4
    for _ in 0..<40_000 {
        if condition() { return }

        precondition(ProcessInfo.processInfo.systemUptime < deadline,
            "Collector fixture did not complete within its deadline")
        CFRunLoopRunInMode(.defaultMode, 0.001, false)
    }
    preconditionFailure("Collector fixture exhausted its bounded event steps")
}

@MainActor
private final class CollectorExitCheck {
    var exit: (Int32, LiveCollectorOutput)?

    func receive(_ event: LiveCollectorEvent) {
        if case .exited(let status, let output) = event { exit = (status, output) }
        if case .failed(let message) = event { preconditionFailure(message) }
    }
}

@MainActor
final class ControlledLiveCollector: LiveCollectorRunning {
    private(set) var starts = 0
    private var event: (@MainActor @Sendable (LiveCollectorEvent) -> Void)?
    var callback: (@MainActor @Sendable (LiveCollectorEvent) -> Void)? { event }

    func start(configuration: LiveCollectorConfiguration,
               event: @escaping @MainActor @Sendable (LiveCollectorEvent) -> Void) {
        starts += 1
        self.event = event
        event(.launched(42))
    }

    func stop(completion: @escaping @MainActor @Sendable () -> Void) {
        completion()
    }

    func fail(_ message: String) { event?(.failed(message)) }

    func exit(status: Int32) {
        event?(.exited(status, LiveCollectorOutput(standard: Data(), error: Data(),
            standardBytes: 0, errorBytes: 0)))
    }
}
