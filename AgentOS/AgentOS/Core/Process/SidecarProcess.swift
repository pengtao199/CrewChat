import Foundation
import os

final class SidecarProcess: @unchecked Sendable {
    private let lock = NSLock()
    private var process: Process?
    private var logHandle: FileHandle?
    private var label = "core"

    private let logger = Logger(subsystem: "com.crewchat.AgentOS", category: "sidecar")

    var isRunning: Bool {
        lock.lock()
        defer { lock.unlock() }
        return process?.isRunning ?? false
    }

    func start(
        binary: URL,
        endpoint: CoreEndpoint,
        dataDirectory: URL,
        logFile: URL,
        extraEnvironment: [String: String] = [:],
        label: String = "core"
    ) throws {
        stop()

        try FileManager.default.createDirectory(at: dataDirectory, withIntermediateDirectories: true)

        if !FileManager.default.fileExists(atPath: logFile.path) {
            FileManager.default.createFile(atPath: logFile.path, contents: nil)
        }
        let handle = try FileHandle(forWritingTo: logFile)
        try handle.seekToEnd()

        let process = Process()
        process.executableURL = binary
        process.currentDirectoryURL = binary.deletingLastPathComponent()

        var environment = ProcessInfo.processInfo.environment
        environment["CODEG_HOST"] = endpoint.host
        environment["CODEG_PORT"] = String(endpoint.port)
        environment["CODEG_TOKEN"] = endpoint.token
        environment["CODEG_DATA_DIR"] = dataDirectory.path
        environment["CODEG_HOME"] = dataDirectory.path
        environment.merge(extraEnvironment) { _, new in new }
        process.environment = environment

        process.standardOutput = handle
        process.standardError = handle
        process.standardInput = FileHandle.nullDevice

        process.terminationHandler = { [logger, label] finished in
            logger.log("\(label) exited: status=\(finished.terminationStatus)")
        }

        try process.run()

        lock.lock()
        self.process = process
        self.logHandle = handle
        self.label = label
        lock.unlock()

        logger.log("\(label) started pid=\(process.processIdentifier) port=\(endpoint.port)")
    }

    func stop() {
        lock.lock()
        let running = process
        process = nil
        let handle = logHandle
        logHandle = nil
        let label = self.label
        lock.unlock()

        guard let running else { return }
        if running.isRunning {
            running.terminate()
            running.waitUntilExit()
        }
        try? handle?.close()
        logger.log("\(label) stopped")
    }
}
