import AppKit
import Foundation
import os
import Security

final class CoreSupervisor: CoreLifecycle, @unchecked Sendable {
    private let lock = NSLock()
    private let sidecar = SidecarProcess()
    private let modelSidecar = SidecarProcess()
    private let tokenStore = TokenStore(directory: CoreBinaryLocator.dataDirectory())
    private let modelTokenStore = TokenStore(directory: CoreBinaryLocator.modelLayerDirectory())
    private let logger = Logger(subsystem: "com.crewchat.AgentOS", category: "supervisor")

    private var continuations: [UUID: AsyncStream<CorePhase>.Continuation] = [:]
    private var phase: CorePhase = .idle
    private var starting = false
    private var ownsCore = false
    private var ownsModelLayer = false
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

        if let adopted = await adoptExistingCore() {
            emit(adopted)
            finishStarting()
            return
        }

        do {
            let binary = try resolveBinary()
            let token = try resolveToken()
            let port = PortPicker.isFree(3080) ? 3080 : PortPicker.freePort()
            let endpoint = CoreEndpoint(host: "127.0.0.1", port: port, token: token)
            let dataDirectory = CoreBinaryLocator.dataDirectory()
            let logFile = CoreBinaryLocator.logFile()

            emit(.connecting)
            let modelEnvironment = await startModelLayerIfAvailable()
            if modelEnvironment.isEmpty && CoreBinaryLocator.isModelLayerRequired() {
                emit(.failed("模型层没有启动，但已标记为必需。删除 \(CoreBinaryLocator.modelLayerRequiredMarker().path) 可回退到 CCR。"))
                finishStarting()
                return
            }
            try sidecar.start(
                binary: binary,
                endpoint: endpoint,
                dataDirectory: dataDirectory,
                logFile: logFile,
                extraEnvironment: modelEnvironment
            )
            ownsCore = true
            CoreRuntimeState.save(port: port, directory: dataDirectory)

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
        stopNow()
        finishStarting()
    }

    func stopNow() {
        if ownsModelLayer {
            modelSidecar.stop()
            ownsModelLayer = false
        }
        if ownsCore {
            sidecar.stop()
            CoreRuntimeState.clear(directory: CoreBinaryLocator.dataDirectory())
            ownsCore = false
        }
        emit(.idle)
    }

    private func adoptExistingCore() async -> CorePhase? {
        let directory = CoreBinaryLocator.dataDirectory()
        guard let state = CoreRuntimeState.load(directory: directory),
              let token = tokenStore.load() else {
            return nil
        }

        let endpoint = CoreEndpoint(host: "127.0.0.1", port: state.port, token: token)
        do {
            let health = try await CoreClient(endpoint: endpoint).health()
            ownsCore = false
            return .connected(endpoint, version: health.version)
        } catch {
            CoreRuntimeState.clear(directory: directory)
            return nil
        }
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

    /// Start the Rust model layer when its binary is present. The ports are
    /// chosen to avoid a running CCR: if 3456/3458 are taken, the sidecar gets
    /// free ports and codeg is pointed at those, so both can run side by side
    /// during the cutover. An empty dictionary means "keep using CCR".
    private func startModelLayerIfAvailable() async -> [String: String] {
        guard let binary = CoreBinaryLocator.locateModelLayer() else {
            return [:]
        }

        let gatewayPort = PortPicker.isFree(3456) ? 3456 : PortPicker.freePort()
        let controlPort = PortPicker.isFree(3458) ? 3458 : PortPicker.freePort()
        let token: String
        do {
            token = try resolveModelToken()
        } catch {
            logger.error("model layer token failed: \(error.localizedDescription)")
            return [:]
        }

        let directory = CoreBinaryLocator.modelLayerDirectory()
        let logFile = CoreBinaryLocator.modelLayerLogFile()
        let disabled = CoreBinaryLocator.dataDirectory()
            .appending(path: "ccr-disabled-models.json")
        let environment = [
            "MODEL_LAYER_DATA_DIR": directory.path,
            "MODEL_LAYER_CONTROL_TOKEN": token,
            "MODEL_LAYER_GATEWAY_PORT": String(gatewayPort),
            "MODEL_LAYER_CONTROL_PORT": String(controlPort),
            "MODEL_LAYER_HOST": "127.0.0.1",
            "MODEL_LAYER_IMPORT_ON_START": "1",
            "MODEL_LAYER_APPLY_PROFILE": CoreBinaryLocator.isModelLayerRequired() ? "1" : "0",
            "MODEL_LAYER_DISABLED_MODELS": disabled.path,
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
            return [:]
        }

        if await waitForModelLayer(port: gatewayPort) {
            ownsModelLayer = true
            logger.log("model layer ready gateway=\(gatewayPort) control=\(controlPort)")
            return [
                "CCR_WEB_URL": "http://127.0.0.1:\(controlPort)",
                "CCR_WEB_AUTH_TOKEN": token,
                "CODEG_CCR_SUPERVISE": "0",
            ]
        }

        modelSidecar.stop()
        logger.error("model layer did not become healthy; falling back to CCR")
        return [:]
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
        if let token = tokenStore.load() {
            return token
        }
        let token = try makeToken()
        try tokenStore.save(token)
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
