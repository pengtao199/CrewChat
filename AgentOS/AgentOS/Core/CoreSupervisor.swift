import AppKit
import Foundation
import os
import Security

final class CoreSupervisor: CoreLifecycle, @unchecked Sendable {
    private let lock = NSLock()
    private let modelSidecar = SidecarProcess()
    private let skillSidecar = SidecarProcess()
    private let cliSidecar = SidecarProcess()
    private let mcpSidecar = SidecarProcess()
    private let modelTokenStore = TokenStore(directory: CoreBinaryLocator.modelLayerDirectory())
    private let cliTokenStore = TokenStore(directory: CoreBinaryLocator.cliLayerDirectory())
    private let logger = Logger(subsystem: "com.crewchat.AgentOS", category: "supervisor")

    private var continuations: [UUID: AsyncStream<CorePhase>.Continuation] = [:]
    private var phase: CorePhase = .idle
    private var starting = false
    private var ownsModelLayer = false
    private var ownsSkillLayer = false
    private var ownsCliLayer = false
    private var ownsMCPLayer = false
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

        emit(.connecting)
        _ = await startModelLayerIfAvailable()
        _ = await startSkillLayerIfAvailable()
        _ = await startMCPLayerIfAvailable()

        if await startCliLayerIfAvailable() {
            finishStarting()
            return
        }

        emit(.failed("cli-layer 没有启动。日志：\(CoreBinaryLocator.cliLayerLogFile().path)"))
        finishStarting()
    }

    func stop() async {
        stopNow()
        finishStarting()
    }

    func stopNow() {
        if ownsCliLayer {
            cliSidecar.stop()
            ownsCliLayer = false
        }
        if ownsSkillLayer {
            skillSidecar.stop()
            ownsSkillLayer = false
        }
        if ownsMCPLayer {
            mcpSidecar.stop()
            ownsMCPLayer = false
        }
        if ownsModelLayer {
            modelSidecar.stop()
            ownsModelLayer = false
        }
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

    /// Start the local skill library before cli-layer so `skill_*` commands
    /// can proxy immediately. A missing/failed skill layer does not block
    /// chat; skill commands surface the error until the daemon is available.
    private func startSkillLayerIfAvailable() async -> Bool {
        if await skillLayerHealthy() {
            logger.log("skill layer already running")
            return true
        }
        guard let binary = CoreBinaryLocator.locateSkillLayer() else {
            logger.log("skill layer binary not found")
            return false
        }

        let home = FileManager.default.homeDirectoryForCurrentUser
        let root = home.appending(path: ".agents", directoryHint: .isDirectory)
        let port = PortPicker.isFree(3460) ? 3460 : PortPicker.freePort()
        let logFile = CoreBinaryLocator.skillLayerLogFile()
        let environment = [
            "SKILL_LAYER_PORT": String(port),
            "SKILL_LAYER_ROOT": root.path,
        ]

        do {
            try skillSidecar.start(
                binary: binary,
                endpoint: CoreEndpoint(host: "127.0.0.1", port: port, token: ""),
                dataDirectory: root,
                logFile: logFile,
                extraEnvironment: environment,
                label: "skill"
            )
        } catch {
            logger.error("skill layer failed to start: \(error.localizedDescription)")
            return false
        }

        if await waitForSkillLayer() {
            ownsSkillLayer = true
            logger.log("skill layer ready on :\(port)")
            return true
        }
        skillSidecar.stop()
        logger.error("skill layer did not become healthy; skill commands will be unavailable")
        return false
    }

    private func skillLayerHealthy() async -> Bool {
        let runtime = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".agents/skill-layer/runtime.json")
        guard let data = try? Data(contentsOf: runtime),
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let urlString = value["url"] as? String,
              let token = value["token"] as? String,
              let url = URL(string: urlString + "/v1/health") else {
            return false
        }
        var request = URLRequest(url: url)
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        guard let (_, response) = try? await URLSession.shared.data(for: request) else {
            return false
        }
        return (response as? HTTPURLResponse)?.statusCode == 200
    }

    private func waitForSkillLayer() async -> Bool {
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline {
            if !skillSidecar.isRunning {
                return false
            }
            if await skillLayerHealthy() {
                return true
            }
            try? await Task.sleep(for: .milliseconds(250))
        }
        return false
    }

    /// The MCP control plane owns the server registry and per-Agent proxies.
    /// It must be ready before cli-layer starts, because cli-layer reads its
    /// runtime file once at boot.
    private func startMCPLayerIfAvailable() async -> Bool {
        if await mcpLayerHealthy() {
            logger.log("mcp layer already running")
            return true
        }
        guard let binary = CoreBinaryLocator.locateMCPLayer() else {
            logger.log("mcp layer binary not found")
            return false
        }

        let root = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".agents/mcp", directoryHint: .isDirectory)
        let port = PortPicker.isFree(3461) ? 3461 : PortPicker.freePort()
        let logFile = CoreBinaryLocator.mcpLayerLogFile()
        let environment = [
            "MCP_LAYER_ROOT": root.path,
            "MCP_LAYER_PORT": String(port),
        ]

        do {
            try mcpSidecar.start(
                binary: binary,
                endpoint: CoreEndpoint(host: "127.0.0.1", port: port, token: ""),
                dataDirectory: root,
                logFile: logFile,
                extraEnvironment: environment,
                label: "mcp"
            )
        } catch {
            logger.error("mcp layer failed to start: \(error.localizedDescription)")
            return false
        }

        if await waitForMCPLayer() {
            ownsMCPLayer = true
            logger.log("mcp layer ready on :\(port)")
            return true
        }
        mcpSidecar.stop()
        logger.error("mcp layer did not become healthy; MCP settings will be unavailable")
        return false
    }

    private func mcpLayerHealthy() async -> Bool {
        let runtime = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: ".agents/mcp/runtime.json")
        guard let data = try? Data(contentsOf: runtime),
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let urlString = value["url"] as? String,
              let token = value["token"] as? String,
              let baseURL = URL(string: urlString) else {
            return false
        }
        var request = URLRequest(url: baseURL.appendingPathComponent("v1/health"))
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              (response as? HTTPURLResponse)?.statusCode == 200,
              let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let capabilities = body["capabilities"] as? [String] else {
            return false
        }
        return capabilities.contains("server-health")
    }

    private func waitForMCPLayer() async -> Bool {
        let deadline = Date().addingTimeInterval(20)
        while Date() < deadline {
            if !mcpSidecar.isRunning {
                return false
            }
            if await mcpLayerHealthy() {
                return true
            }
            try? await Task.sleep(for: .milliseconds(250))
        }
        return false
    }

    /// Start cli-layer. It is the only core now; a failure is surfaced to the UI.
    private func startCliLayerIfAvailable() async -> Bool {
        guard let binary = CoreBinaryLocator.locateCliLayer() else {
            logger.log("cli layer enabled but binary not found")
            return false
        }
        let port = PortPicker.isFree(3081) ? 3081 : PortPicker.freePort()
        let token: String
        do {
            if let existing = cliTokenStore.load() {
                token = existing
            } else {
                token = try makeToken()
                try cliTokenStore.save(token)
            }
        } catch {
            logger.error("cli layer token failed: \(error.localizedDescription)")
            return false
        }

        let directory = CoreBinaryLocator.cliLayerDirectory()
        let logFile = CoreBinaryLocator.cliLayerLogFile()
        let model = readModelLayerRuntime()
        let environment = [
            "CLI_LAYER_DATA_DIR": directory.path,
            "CLI_LAYER_PORT": String(port),
            "CLI_LAYER_TOKEN": token,
            "MODEL_LAYER_URL": model.controlUrl,
            "MODEL_LAYER_GATEWAY_URL": model.gatewayUrl,
            "MODEL_LAYER_TOKEN": model.token ?? "",
        ]

        do {
            try cliSidecar.start(
                binary: binary,
                endpoint: CoreEndpoint(host: "127.0.0.1", port: port, token: token),
                dataDirectory: directory,
                logFile: logFile,
                extraEnvironment: environment,
                label: "cli"
            )
        } catch {
            logger.error("cli layer failed to start: \(error.localizedDescription)")
            return false
        }

        if await waitForCliLayer(port: port, token: token) {
            ownsCliLayer = true
            logger.log("cli layer ready on :\(port)")
            emit(.connected(CoreEndpoint(host: "127.0.0.1", port: port, token: token), version: "cli-layer"))
            return true
        }

        cliSidecar.stop()
        logger.error("cli layer did not become healthy")
        return false
    }

    private func waitForCliLayer(port: Int, token: String) async -> Bool {
        let deadline = Date().addingTimeInterval(20)
        guard let url = URL(string: "http://127.0.0.1:\(port)/api/health") else {
            return false
        }
        while Date() < deadline {
            if !cliSidecar.isRunning {
                return false
            }
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = Data("{}".utf8)
            if let (_, response) = try? await URLSession.shared.data(for: request),
               (response as? HTTPURLResponse)?.statusCode == 200 {
                return true
            }
            try? await Task.sleep(for: .milliseconds(250))
        }
        return false
    }

    private func readModelLayerRuntime() -> (controlUrl: String, token: String?, gatewayUrl: String) {
        let path = CoreBinaryLocator.modelLayerDirectory().appending(path: "model-layer.json")
        if let data = try? Data(contentsOf: path),
           let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            let control = value["controlUrl"] as? String ?? "http://127.0.0.1:3458"
            let gateway = value["gatewayUrl"] as? String ?? "http://127.0.0.1:3456"
            return (control, value["controlToken"] as? String, gateway)
        }
        return ("http://127.0.0.1:3458", nil, "http://127.0.0.1:3456")
    }

    /// Start the Rust model layer when its binary is present. If it is not
    /// present, cli-layer still starts and each CLI falls back to its own
    /// configured model.
    private func startModelLayerIfAvailable() async -> Bool {
        guard let binary = CoreBinaryLocator.locateModelLayer() else {
            logger.log("model layer binary not found; CLI engines will use their own models")
            return false
        }

        let gatewayPort = PortPicker.isFree(3456) ? 3456 : PortPicker.freePort()
        let controlPort = PortPicker.isFree(3458) ? 3458 : PortPicker.freePort()
        let token: String
        do {
            token = try resolveModelToken()
        } catch {
            logger.error("model layer token failed: \(error.localizedDescription)")
            return false
        }

        let directory = CoreBinaryLocator.modelLayerDirectory()
        let logFile = CoreBinaryLocator.modelLayerLogFile()
        let environment = [
            "MODEL_LAYER_DATA_DIR": directory.path,
            "MODEL_LAYER_CONTROL_TOKEN": token,
            "MODEL_LAYER_GATEWAY_PORT": String(gatewayPort),
            "MODEL_LAYER_CONTROL_PORT": String(controlPort),
            "MODEL_LAYER_HOST": "127.0.0.1",
        ]

        do {
            try modelSidecar.start(
                binary: binary,
                endpoint: CoreEndpoint(host: "127.0.0.1", port: controlPort, token: token),
                dataDirectory: directory,
                logFile: logFile,
                extraEnvironment: environment,
                label: "model"
            )
        } catch {
            logger.error("model layer failed to start: \(error.localizedDescription)")
            return false
        }

        if await waitForModelLayer(port: gatewayPort) {
            ownsModelLayer = true
            logger.log("model layer ready gateway=\(gatewayPort) control=\(controlPort)")
            return true
        }

        modelSidecar.stop()
        logger.error("model layer did not become healthy; CLI engines will use their own models")
        return false
    }

    private func waitForModelLayer(port: Int) async -> Bool {
        let deadline = Date().addingTimeInterval(15)
        while Date() < deadline {
            if !modelSidecar.isRunning {
                return false
            }
            if let url = URL(string: "http://127.0.0.1:\(port)/health"),
               let (_, response) = try? await URLSession.shared.data(from: url),
               (response as? HTTPURLResponse)?.statusCode == 200 {
                return true
            }
            try? await Task.sleep(for: .milliseconds(250))
        }
        return false
    }

    private func resolveModelToken() throws -> String {
        if let token = modelTokenStore.load() {
            return token
        }
        let token = try makeToken()
        try modelTokenStore.save(token)
        return token
    }

    private func makeToken() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 24)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        guard status == errSecSuccess else {
            throw CoreError(code: "token_generation", message: "无法生成访问令牌（\(status)）", detail: nil)
        }
        return bytes.map { String(format: "%02x", $0) }.joined()
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
