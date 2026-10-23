import Foundation

@MainActor
@Observable
final class EnginesSettingsViewModel {
    private(set) var engines: [Engine] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    /// One install at a time: npm/uv share global prefixes, and two parallel
    /// installs are far more likely to trip over each other than to help.
    private(set) var installing: String?
    private(set) var installError: String?

    /// Newer release per agent, only when it is actually newer than the
    /// installed version. `latestChecked` distinguishes "up to date" (present
    /// in the set, no entry) from "not asked yet".
    private(set) var latestVersions: [String: String] = [:]
    private(set) var latestChecked: Set<String> = []
    private(set) var latestCheckFailed: Set<String> = []
    private(set) var checkingLatest: Set<String> = []

    private(set) var diagnostics: [String: EngineDiagnostics] = [:]
    private(set) var diagnosticsLoading: Set<String> = []
    private(set) var diagnosticsError: [String: String] = [:]

    private let client: CoreEngines

    init(engines: CoreEngines) {
        self.client = engines
    }

    var usable: [Engine] {
        engines.filter(\.isUsable)
    }

    var notInstalled: [Engine] {
        engines.filter { !$0.isUsable }
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            engines = try await client.engines()
            await enableAllIfNeeded()
        } catch {
            engines = []
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func clearInstallError() {
        installError = nil
    }

    func latest(for engine: Engine) -> String? {
        latestVersions[engine.agentType]
    }

    func isCheckingLatest(_ engine: Engine) -> Bool {
        checkingLatest.contains(engine.agentType)
    }

    func checkLatest(for engine: Engine, force: Bool = false) async {
        guard engine.isInstalled else { return }
        if !force, latestChecked.contains(engine.agentType) { return }
        if checkingLatest.contains(engine.agentType) { return }

        checkingLatest.insert(engine.agentType)
        defer { checkingLatest.remove(engine.agentType) }

        do {
            let release = try await client.latestRelease(agentType: engine.agentType)
            if let release, Self.isNewer(release, than: engine.installedVersion) {
                latestVersions[engine.agentType] = release
            } else {
                latestVersions.removeValue(forKey: engine.agentType)
            }
            latestCheckFailed.remove(engine.agentType)
            latestChecked.insert(engine.agentType)
        } catch {
            // An update check is best-effort: a flaky registry must not turn
            // the page into an error state or block installing from scratch,
            // but it must not claim "up to date" either.
            latestVersions.removeValue(forKey: engine.agentType)
            latestCheckFailed.insert(engine.agentType)
            latestChecked.insert(engine.agentType)
        }
    }

    func install(_ engine: Engine, version: String?) async {
        guard installing == nil else { return }

        installing = engine.agentType
        installError = nil
        defer { installing = nil }

        let taskID = UUID().uuidString

        do {
            if engine.distributionType == "binary" {
                try await client.downloadAgentBinary(
                    agentType: engine.agentType,
                    version: version,
                    taskID: taskID
                )
            } else {
                try await client.prepareAgent(
                    agentType: engine.agentType,
                    registryVersion: engine.registryVersion,
                    version: version,
                    cleanFirst: version != nil,
                    taskID: taskID
                )

                // Pi is two installs in codeg: the pi-acp adapter (npx) and
                // the `pi` coding-agent binary it resolves from PATH.
                if engine.agentType == "pi" {
                    try await client.installPiBinary(taskID: UUID().uuidString)
                }
            }

            latestVersions.removeValue(forKey: engine.agentType)
            latestChecked.remove(engine.agentType)
            await load()
        } catch {
            installError = error.localizedDescription
        }
    }

    func loadDiagnostics(for engine: Engine, force: Bool = false) async {
        if !force, diagnostics[engine.agentType] != nil { return }
        if diagnosticsLoading.contains(engine.agentType) { return }

        diagnosticsLoading.insert(engine.agentType)
        diagnosticsError.removeValue(forKey: engine.agentType)
        defer { diagnosticsLoading.remove(engine.agentType) }

        do {
            diagnostics[engine.agentType] = try await client.diagnostics(
                agentType: engine.agentType
            )
        } catch {
            diagnosticsError[engine.agentType] = error.localizedDescription
        }
    }

    func update(_ engine: Engine, enabled: Bool, env: [String: String]) async throws {
        try await client.setPreferences(
            EnginePreferences(
                agentType: engine.agentType,
                enabled: enabled,
                env: env
            )
        )
        await load()
    }

    /// The product no longer exposes a per-engine enable switch: every
    /// built-in CLI that can run on this platform is on. Re-enable anything a
    /// previous build left off so removing the toggle cannot strand an engine
    /// in a disabled state the UI can no longer show.
    private func enableAllIfNeeded() async {
        let disabled = engines.filter { !$0.enabled && $0.available }
        guard !disabled.isEmpty else { return }

        for engine in disabled {
            try? await client.setPreferences(
                EnginePreferences(
                    agentType: engine.agentType,
                    enabled: true,
                    env: engine.env
                )
            )
        }

        if let refreshed = try? await client.engines() {
            engines = refreshed
        }
    }

    static func isNewer(_ candidate: String, than current: String?) -> Bool {
        guard let current, !current.isEmpty else { return true }

        let lhs = versionParts(candidate)
        let rhs = versionParts(current)
        let count = max(lhs.count, rhs.count)

        for index in 0..<count {
            let left = index < lhs.count ? lhs[index] : 0
            let right = index < rhs.count ? rhs[index] : 0
            if left != right { return left > right }
        }

        return false
    }

    private static func versionParts(_ value: String) -> [Int] {
        value
            .trimmingCharacters(in: CharacterSet(charactersIn: "vV"))
            .split(whereSeparator: { $0 == "." || $0 == "-" || $0 == "+" })
            .map { part in
                Int(part.prefix(while: \.isNumber)) ?? 0
            }
    }
}
