import Darwin
import Foundation

enum LiveCollectorChild {
    static func spawn(configuration: LiveCollectorConfiguration,
                      standard: Int32, diagnostic: Int32) throws -> Int32 {
        guard configuration.arguments.count <= 64 else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(E2BIG), userInfo: nil)
        }

        var actions: posix_spawn_file_actions_t?
        try check(posix_spawn_file_actions_init(&actions))
        defer { report(posix_spawn_file_actions_destroy(&actions)) }

        try check(posix_spawn_file_actions_addopen(&actions, STDIN_FILENO, "/dev/null", O_RDONLY, 0))
        try check(posix_spawn_file_actions_adddup2(&actions, standard, STDOUT_FILENO))
        try check(posix_spawn_file_actions_adddup2(&actions, diagnostic, STDERR_FILENO))
        if standard > STDERR_FILENO {
            try check(posix_spawn_file_actions_addclose(&actions, standard))
        }
        if diagnostic > STDERR_FILENO {
            try check(posix_spawn_file_actions_addclose(&actions, diagnostic))
        }

        var attributes: posix_spawnattr_t?
        try check(posix_spawnattr_init(&attributes))
        defer { report(posix_spawnattr_destroy(&attributes)) }

        try configure(&attributes)

        return try arguments(configuration) { values in
            var identifier: pid_t = 0
            try check(posix_spawn(&identifier, configuration.executable.path,
                &actions, &attributes, values, environ))

            return identifier
        }
    }

    private static func configure(_ attributes: inout posix_spawnattr_t?) throws {
        var defaults = sigset_t()
        try checkSignal(sigemptyset(&defaults))
        try checkSignal(sigaddset(&defaults, SIGTERM))
        try checkSignal(sigaddset(&defaults, SIGINT))
        try checkSignal(sigaddset(&defaults, SIGPIPE))
        try check(posix_spawnattr_setsigdefault(&attributes, &defaults))

        var mask = sigset_t()
        try checkSignal(sigemptyset(&mask))
        try check(posix_spawnattr_setsigmask(&attributes, &mask))
        let flags = Int16(POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK
            | POSIX_SPAWN_CLOEXEC_DEFAULT)

        try check(posix_spawnattr_setflags(&attributes, flags))
    }

    private static func arguments(_ configuration: LiveCollectorConfiguration,
                                  body: (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) throws
                                  -> Int32) throws -> Int32 {
        let strings = [configuration.executable.path] + configuration.arguments
        var values = [UnsafeMutablePointer<CChar>?]()
        values.reserveCapacity(strings.count + 1)
        defer {
            for value in values { free(value) }
        }

        for string in strings {
            guard string.utf8.count <= 4096, !string.utf8.contains(0) else {
                throw NSError(domain: NSPOSIXErrorDomain, code: Int(E2BIG), userInfo: nil)
            }

            guard let value = strdup(string) else {
                throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno), userInfo: nil)
            }

            values.append(value)
        }
        values.append(nil)

        return try values.withUnsafeMutableBufferPointer { buffer in
            guard let base = buffer.baseAddress else {
                preconditionFailure("Collector argv is nonempty")
            }

            return try body(base)
        }
    }

    private static func check(_ status: Int32) throws {
        guard status == 0 else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(status), userInfo: nil)
        }
    }

    private static func checkSignal(_ status: Int32) throws {
        guard status == 0 else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno), userInfo: nil)
        }
    }

    private static func report(_ status: Int32) {
        if status != 0 {
            fputs("Flightdeck spawn resource cleanup failed: \(status)\n", stderr)
        }
    }
}
