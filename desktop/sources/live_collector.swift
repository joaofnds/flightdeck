import AppKit
import Combine
import Darwin
import Foundation

@MainActor
final class LiveCollectorSession: ObservableObject {
    enum State: Equatable {
        case idle, starting, running, stopping, stopped
        case failed(String)
    }

    @Published private(set) var state = State.idle
    private let configuration: LiveCollectorConfiguration
    private let process: any LiveCollectorRunning
    private let pollingEnabled: Bool
    private let now: () -> Double
    private let onFailure: @MainActor () -> Void
    private var deadline = 0.0
    private var witnessed: RecordedObservationIdentity?
    private var timer: AnyCancellable?
    private var processFailure: String?
    private var launchedPID: UInt32?
    private var observedProducerPID: UInt32?
    private var attemptGeneration: UInt64 = 0

    init(configuration: LiveCollectorConfiguration = .bundled(),
         process: any LiveCollectorRunning = LiveCollectorProcess(),
         pollingEnabled: Bool = true,
         now: @escaping () -> Double = { ProcessInfo.processInfo.systemUptime },
         onFailure: @escaping @MainActor () -> Void = {}) {
        self.configuration = configuration
        self.process = process
        self.pollingEnabled = pollingEnabled
        self.now = now
        self.onFailure = onFailure
    }

    func start() {
        guard state == .idle else { return }

        begin()
    }

    func retry() {
        guard case .failed = state else { return }

        begin()
    }

    private func begin() {
        attemptGeneration += 1
        let generation = attemptGeneration

        state = .starting
        witnessed = nil
        launchedPID = nil
        observedProducerPID = nil
        processFailure = nil
        deadline = now() + 20

        process.start(configuration: configuration) { [weak self] event in
            guard let self else { return }
            guard generation == attemptGeneration else { return }

            receive(event)
        }

        if pollingEnabled {
            timer = Timer.publish(every: 0.25, on: .main, in: .common)
                .autoconnect().sink { [weak self] _ in self?.step() }
        }
    }

    private func receive(_ event: LiveCollectorEvent) {
        let ownsProducer = launchedPID != nil && launchedPID == observedProducerPID

        switch event {
        case .launched(let processIdentifier):
            launchedPID = UInt32(exactly: processIdentifier)
        case .failed(let message):
            processFailure = message
            launchedPID = nil
        case .exited(let status, let output):
            processFailure = "Collector exited (\(status)). \(output.errorText)"
            launchedPID = nil
        }

        if state == .running, ownsProducer, let processFailure {
            fail(processFailure)
        }
    }

    func observe(identity: RecordedObservationIdentity, collectorPID: UInt32,
                 collectorCompleted: Bool) {
        step()
        guard state == .starting || state == .running else { return }

        let previousProducerPID = observedProducerPID

        observedProducerPID = collectorPID
        guard !collectorCompleted else {
            witnessed = nil
            return
        }

        guard state == .starting else { return }

        if let previous = witnessed, previous.run == identity.run,
           previous.boot == identity.boot, previous.publication < identity.publication,
           previousProducerPID == collectorPID {
            state = .running
            timer = nil
        } else {
            witnessed = identity
        }
    }

    func step() {
        guard state == .starting, now() >= deadline else { return }

        fail(processFailure ?? "Live readings did not advance within 20 seconds.")
    }

    private func fail(_ message: String) {
        if case .failed = state { return }

        timer = nil
        state = .failed(message)

        onFailure()
    }

    func stop(completion: @escaping @MainActor @Sendable () -> Void) {
        precondition(state != .stopping)
        guard state != .stopped else {
            completion()
            return
        }

        timer = nil
        state = .stopping
        process.stop { [weak self] in
            self?.state = .stopped
            completion()
        }
    }
}

struct LiveCollectorConfiguration: Sendable {
    let executable: URL
    let arguments: [String]

    static var defaultRecordingURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Flightdeck/flightdeck.recording")
    }

    static func bundled() -> Self {
        let executable = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/collector")

        return Self(executable: executable,
            arguments: ["--ring", "flightdeck", "--recording", defaultRecordingURL.path,
                        "--parent-pid", String(getpid())])
    }
}

enum LiveCollectorEvent: Sendable {
    case launched(Int32)
    case failed(String)
    case exited(Int32, LiveCollectorOutput)
}

struct LiveCollectorOutput: Sendable {
    let standard: Data
    let error: Data
    let standardBytes: UInt64
    let errorBytes: UInt64
    var errorText: String { String(decoding: error, as: UTF8.self) }
}

@MainActor
protocol LiveCollectorRunning: AnyObject {
    func start(configuration: LiveCollectorConfiguration,
               event: @escaping @MainActor @Sendable (LiveCollectorEvent) -> Void)
    func stop(completion: @escaping @MainActor @Sendable () -> Void)
}

@MainActor
final class LiveCollectorProcess: LiveCollectorRunning {
    private let worker: LiveCollectorWorker

    init(shutdownGrace: Double = 125) {
        precondition(shutdownGrace > 0)
        precondition(shutdownGrace <= 125)

        worker = LiveCollectorWorker(shutdownGrace: shutdownGrace)
    }

    func start(configuration: LiveCollectorConfiguration,
               event: @escaping @MainActor @Sendable (LiveCollectorEvent) -> Void) {
        worker.start(configuration: configuration, event: event)
    }

    func stop(completion: @escaping @MainActor @Sendable () -> Void) {
        worker.stop(completion: completion)
    }
}

private final class LiveCollectorWorker: @unchecked Sendable {
    private let queue = DispatchQueue(label: "flightdeck.collector", qos: .utility)
    private var processIdentifier: Int32?
    private var output: LiveCollectorPipe?
    private var error: LiveCollectorPipe?
    private var completion: (@MainActor @Sendable () -> Void)?
    private var event: (@MainActor @Sendable (LiveCollectorEvent) -> Void)?
    private var timer: DispatchSourceTimer?
    private var stopping = false
    private var generation: UInt64 = 0
    private let shutdownGrace: Double

    init(shutdownGrace: Double) { self.shutdownGrace = shutdownGrace }

    func start(configuration: LiveCollectorConfiguration,
               event: @escaping @MainActor @Sendable (LiveCollectorEvent) -> Void) {
        queue.async { [self] in
            guard !stopping else { return }

            self.event = event
            if let processIdentifier {
                DispatchQueue.main.async { event(.launched(processIdentifier)) }
                return
            }

            generation += 1
            do {
                let standard = try LiveCollectorPipe(queue: queue)
                output = standard
                let diagnostic = try LiveCollectorPipe(queue: queue)
                error = diagnostic
                let identifier = try LiveCollectorChild.spawn(configuration: configuration,
                    standard: standard.pipe.fileHandleForWriting.fileDescriptor,
                    diagnostic: diagnostic.pipe.fileHandleForWriting.fileDescriptor)
                processIdentifier = identifier

                let reaper = DispatchSource.makeTimerSource(queue: queue)
                reaper.schedule(deadline: .now() + .milliseconds(250),
                    repeating: .milliseconds(250), leeway: .milliseconds(25))
                reaper.setEventHandler { [weak self] in self?.reap() }
                timer = reaper
                reaper.activate()

                try standard.closeWriter()
                try diagnostic.closeWriter()
                DispatchQueue.main.async { event(.launched(identifier)) }
            } catch let failure {
                if processIdentifier == nil { closeOutput() }
                if let processIdentifier {
                    reportSignal(processIdentifier, signal: SIGTERM)
                }

                DispatchQueue.main.async { event(.failed(failure.localizedDescription)) }
            }
        }
    }

    func stop(completion: @escaping @MainActor @Sendable () -> Void) {
        queue.async { [self] in
            precondition(!stopping)

            stopping = true
            self.completion = completion
            reap()
            guard let processIdentifier else {
                if stopping { stopCompleted() }
                return
            }

            reportSignal(processIdentifier, signal: SIGTERM)
            let token = generation
            queue.asyncAfter(deadline: .now() + shutdownGrace) { [self] in
                guard stopping, generation == token else { return }

                reap()
                guard let processIdentifier = self.processIdentifier else { return }

                reportSignal(processIdentifier, signal: SIGKILL)
                queue.asyncAfter(deadline: .now() + 2) { [self] in
                    guard stopping, generation == token else { return }

                    reap()
                    if stopping {
                        fputs("Flightdeck collector did not exit after SIGKILL.\n", stderr)
                        stopCompleted()
                    }
                }
            }
        }
    }

    private func reap() {
        dispatchPrecondition(condition: .onQueue(queue))
        guard let processIdentifier else { return }

        var status: Int32 = 0
        let result = waitpid(processIdentifier, &status, WNOHANG)

        if result == processIdentifier {
            let signal = status & 0x7f
            if signal == 0x7f { return }

            if signal == 0 {
                finish(status: (status >> 8) & 0xff)
            } else {
                finish(status: signal)
            }
        } else {
            if result < 0, errno != EINTR {
                let failure = errno
                self.processIdentifier = nil
                closeOutput()
                timer?.cancel()
                timer = nil
                if let event {
                    DispatchQueue.main.async {
                        event(.failed("Could not reap owned collector: \(failure)."))
                    }
                }
                if stopping { stopCompleted() }
            }
        }
    }

    private func reportSignal(_ identifier: Int32, signal: Int32) {
        // Only this queue reaps this child, so its unreaped PID cannot be reused before kill.
        dispatchPrecondition(condition: .onQueue(queue))
        precondition(identifier == processIdentifier)

        let status = kill(identifier, signal)
        if status != 0, errno != ESRCH {
            fputs("Flightdeck could not signal its collector: \(errno)\n", stderr)
        }
    }

    private func closeOutput() {
        output?.close()
        error?.close()
        output = nil
        error = nil
    }

    private func finish(status: Int32) {
        output?.drain()
        error?.drain()
        let captured = LiveCollectorOutput(standard: output?.tail ?? Data(),
            error: error?.tail ?? Data(), standardBytes: output?.bytes ?? 0,
            errorBytes: error?.bytes ?? 0)

        closeOutput()
        timer?.cancel()
        timer = nil
        processIdentifier = nil

        if let event {
            DispatchQueue.main.async { event(.exited(status, captured)) }
        }
        if stopping { stopCompleted() }
    }

    private func stopCompleted() {
        guard let completion else { preconditionFailure("Missing collector stop completion") }

        self.completion = nil
        stopping = false
        DispatchQueue.main.async { completion() }
    }
}

private final class LiveCollectorPipe: @unchecked Sendable {
    static let tailBytesMax = 16_384
    let pipe = Pipe()
    private(set) var bytes: UInt64 = 0
    private var source: DispatchSourceRead?
    private var buffer = [UInt8](repeating: 0, count: 4096)
    private var storage = [UInt8](repeating: 0, count: tailBytesMax)
    private var tailCount = 0
    private var tailNext = 0
    private var closed = false
    private var readingEnded = false
    private var writerClosed = false

    var tail: Data {
        let start = tailCount == Self.tailBytesMax ? tailNext : 0
        let firstCount = min(tailCount, Self.tailBytesMax - start)
        var result = Data(storage[start..<(start + firstCount)])
        result.append(contentsOf: storage.prefix(tailCount - firstCount))

        return result
    }

    init(queue: DispatchQueue) throws {
        let descriptor = pipe.fileHandleForReading.fileDescriptor
        let flags = fcntl(descriptor, F_GETFL)
        guard flags != -1, fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) != -1 else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno), userInfo: nil)
        }

        let reader = DispatchSource.makeReadSource(fileDescriptor: descriptor, queue: queue)
        let handle = pipe.fileHandleForReading
        reader.setCancelHandler {
            do { try handle.close() }
            catch { fputs("Flightdeck collector output close failed: \(error)\n", stderr) }
        }
        reader.setEventHandler { [weak self] in self?.drain() }
        source = reader
        reader.activate()
    }

    func drain() {
        guard !closed, !readingEnded else { return }

        for _ in 0..<32 {
            let count = Darwin.read(pipe.fileHandleForReading.fileDescriptor, &buffer, buffer.count)
            if count > 0 {
                bytes += UInt64(count)
                for index in 0..<count {
                    storage[tailNext] = buffer[index]
                    tailNext = (tailNext + 1) % Self.tailBytesMax
                }
                tailCount = min(Self.tailBytesMax, tailCount + count)
            } else {
                if count == 0 {
                    readingEnded = true
                    source?.cancel()
                }
                if count < 0, errno != EAGAIN, errno != EINTR {
                    fputs("Flightdeck collector output read failed: \(errno)\n", stderr)
                }
                return
            }
        }
    }

    func closeWriter() throws {
        try pipe.fileHandleForWriting.close()
        writerClosed = true
    }

    func close() {
        guard !closed else { return }

        closed = true
        source?.cancel()
        source = nil
        if !writerClosed {
            do { try closeWriter() }
            catch { fputs("Flightdeck collector input close failed: \(error)\n", stderr) }
        }
    }
}

@MainActor
final class FlightdeckLifecycle: NSObject, NSApplicationDelegate {
    private var recording: RecordingModel?
    private var collector: LiveCollectorSession?
    private var terminationPending = false

    func configure(recording: RecordingModel, collector: LiveCollectorSession) {
        self.recording = recording
        self.collector = collector
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminationPending else { return .terminateLater }

        recording?.cancel()
        guard let collector else { return .terminateNow }

        terminationPending = true
        collector.stop { sender.reply(toApplicationShouldTerminate: true) }

        return .terminateLater
    }
}
