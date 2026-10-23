import Foundation

protocol CoreModel: Sendable {
    func health() async throws -> ModelHealth
    func start() async throws
    func providers() async throws -> [ModelProvider]
    func presets() async throws -> [ModelProviderPreset]
    func accounts(provider: String, forceRefresh: Bool) async throws -> [ModelAccount]
    func saveProvider(_ draft: ModelProviderDraft) async throws -> ModelProvider
    func deleteProvider(id: String) async throws
    func setModelEnabled(providerID: String, model: String, enabled: Bool) async throws
    func setCredentialEnabled(providerID: String, credentialID: String, enabled: Bool) async throws
    func probeProvider(baseURL: String, apiKey: String?) async throws -> ModelProbeResult
}
