import Foundation

protocol CoreEngines: Sendable {
    func engines() async throws -> [Engine]
    func setEngineEnabled(_ agentType: String, enabled: Bool) async throws
    func setPreferences(_ preferences: EnginePreferences) async throws
}
