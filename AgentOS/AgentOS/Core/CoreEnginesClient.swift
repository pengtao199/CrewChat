import Foundation

final class CoreEnginesClient: CoreEngines, @unchecked Sendable {
    private let provider: CoreClientProvider

    /// Install calls stream npm/uv output and can legitimately run for
    /// minutes; the default 60s client timeout would abort them mid-download.
    private let installTimeout: TimeInterval = 600

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func engines() async throws -> [Engine] {
        let rows: [CoreAcpAgent] = try await provider.client().send(
            "acp_list_agents",
            body: EmptyBody()
        )
        return rows.map(\.domain)
    }

    func setEngineEnabled(_ agentType: String, enabled: Bool) async throws {
        let current = try await engines().first { $0.agentType == agentType }
        try await setPreferences(
            EnginePreferences(
                agentType: agentType,
                enabled: enabled,
                env: current?.env ?? [:]
            )
        )
    }

    func setPreferences(_ preferences: EnginePreferences) async throws {
        let saved = CoreAgentPreferences(
            agentType: preferences.agentType,
            enabled: preferences.enabled,
            env: preferences.env
        )
        try await provider.client().sendIgnoring(
            "acp_update_agent_preferences",
            body: saved
        )
    }

    func latestRelease(agentType: String) async throws -> String? {
        let release: CoreAgentLatestRelease? = try await provider.client().send(
            "acp_fetch_agent_latest_release",
            body: CoreDiagnosticsParams(agentType: agentType),
            timeout: 120
        )
        return release?.version
    }

    func prepareAgent(
        agentType: String,
        registryVersion: String?,
        version: String?,
        cleanFirst: Bool,
        taskID: String
    ) async throws {
        // Returns the resolved version string; the list reload is the truth,
        // so the body is intentionally discarded here.
        let _: String = try await provider.client().send(
            "acp_prepare_npx_agent",
            body: CorePrepareAgentBody(
                agentType: agentType,
                registryVersion: registryVersion,
                version: version,
                cleanFirst: cleanFirst,
                taskId: taskID
            ),
            timeout: installTimeout
        )
    }

    func downloadAgentBinary(
        agentType: String,
        version: String?,
        taskID: String
    ) async throws {
        try await provider.client().sendIgnoring(
            "acp_download_agent_binary",
            body: CoreDownloadAgentBody(
                agentType: agentType,
                version: version,
                taskId: taskID
            ),
            timeout: installTimeout
        )
    }

    func installPiBinary(taskID: String) async throws {
        try await provider.client().sendIgnoring(
            "acp_install_pi_binary",
            body: CoreAgentTaskBody(taskId: taskID),
            timeout: installTimeout
        )
    }

    func diagnostics(agentType: String) async throws -> EngineDiagnostics {
        let report: CoreDiagnosticsReport = try await provider.client().send(
            "acp_env_diagnostics",
            body: CoreDiagnosticsParams(agentType: agentType),
            timeout: 120
        )
        return report.domain
    }
}
