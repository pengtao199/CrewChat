import AppKit
import Foundation
import os
import Security

final class CoreSupervisor: CoreLifecycle, @unchecked Sendable {
    private let lock = NSLock()
    private let sidecar = SidecarProcess()
    private let keychain = KeychainStore(service: "com.crewchat.AgentOS")
    private let logger = Logger(subsystem: "com.crewchat.AgentOS", category: "supervisor")

    private var continuations: [UUID: AsyncStream<CorePhase>.Continuation] = [:]
    private var phase: CorePhase = .idle
    private var starting = false
    private let signalSource: DispatchSourceSignal

    init() {
        signal(SIGTERM, SIG_IGN)
        signalSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
        signalSource.setEventHandler {
            NSApplication.shared.terminate(nil)
        }
        signalSource.resume()

        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.stopNow()
        }
    }

    func phases() -> AsyncStream<CorePhase> {
        let id = UUID()
        let (stream, continuation) = AsyncStream.makeStream(of: CorePhase.self)

        lock.lock()
        continuations[id] = continuation
        let snapshot = phase
        lock.unlock()

        continuation.yield(snapshot)
        continuation.onTermination = { [weak self] _ in
            self?.removeContinuation(id)
        }
        return stream
    }

    func currentEndpoint() -> CoreEndpoint? {
        lock.lock()
        defer { lock.unlock() }
        return phase.endpoint
    }

    func start() async {
        guard beginStarting() else { return }

        do {
            let binary = try resolveBinary()
            let token = try resolveToken()
            let port = PortPicker.isFree(3080) ? 3080 : PortPicker.freePort()
            let endpoint = CoreEndpoint(host: "127.0.0.1", port: port, token: token)
            let dataDirectory = CoreBinaryLocator.dataDirectory()
            let logFile = CoreBinaryLocator.logFile()

            emit(.connecting)
            try sidecar.start(binary: binary, endpoint: endpoint, dataDirectory: dataDirectory, logFile: logFile)

            let client = CoreClient(endpoint: endpoint)
            let deadline = Date().addingTimeInterval(60)

            while Date() < deadline {
                if !sidecar.isRunning {
                    emit(.failed("核心进程已退出。日志：\(logFile.path)"))
                    finishStarting()
                    return
                }
                do {
                    let health = try await client.health()
                    emit(.connected(endpoint, version: health.version))
                    finishStarting()
                    return
                } catch {
                    try? await Task.sleep(for: .milliseconds(300))
                }
            }

            sidecar.stop()
            emit(.failed("核心 60 秒内没有响应。日志：\(logFile.path)"))
        } catch {
            emit(.failed(error.localizedDescription))
        }

        finishStarting()
    }

    func stop() async {
        sidecar.stop()
        emit(.idle)
        finishStarting()
    }

    func stopNow() {
        sidecar.stop()
        emit(.idle)
    }

    private func beginStarting() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !starting else { return false }
        starting = true
        return true
    }

    private func finishStarting() {
        lock.lock()
        starting = false
        lock.unlock()
    }

    private func resolveBinary() throws -> URL {
        guard let binary = CoreBinaryLocator.locate() else {
            throw CoreError(
                code: "core_binary_missing",
                message: "没有找到 codeg-server。把它放进 App 的 Contents/MacOS，或用环境变量 AGENTOS_CORE_BINARY 指定路径。",
                detail: nil
            )
        }
        return binary
    }

    private func resolveToken() throws -> String {
        if let data = keychain.data(for: "core.token"),
           let token = String(data: data, encoding: .utf8),
           !token.isEmpty {
            return token
        }

        var bytes = [UInt8](repeating: 0, count: 24)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        guard status == errSecSuccess else {
            throw CoreError(code: "token_generation", message: "无法生成访问令牌（\(status)）", detail: nil)
        }

        let token = bytes.map { String(format: "%02x", $0) }.joined()
        try keychain.set(Data(token.utf8), for: "core.token")
        return token
    }

    private func emit(_ next: CorePhase) {
        lock.lock()
        phase = next
        let targets = continuations.values
        lock.unlock()

        logger.log("phase: \(String(describing: next), privacy: .public)")
        for continuation in targets {
            continuation.yield(next)
        }
    }

    private func removeContinuation(_ id: UUID) {
        lock.lock()
        continuations[id] = nil
        lock.unlock()
    }
}
