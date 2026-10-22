import Foundation

protocol CoreEngines: Sendable {
    func engines() async throws -> [Engine]
    func setEngineEnabled(_ agentType: String, enabled: Bool) async throws
    func setPreferences(_ preferences: EnginePreferences) async throws
    func latestRelease(agentType: String) async throws -> String?
    func prepareAgent(
        agentType: String,
        registryVersion: String?,
        version: String?,
        cleanFirst: Bool,
        taskID: String
    ) async throws
    func downloadAgentBinary(
        agentType: String,
        version: String?,
        taskID: String
    ) async throws
    func installPiBinary(taskID: String) async throws
    func diagnostics(agentType: String) async throws -> EngineDiagnostics
}
