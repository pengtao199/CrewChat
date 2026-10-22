import Foundation

protocol CoreCCR: Sendable {
    func health() async throws -> CCRHealth
    func start() async throws
    func providers() async throws -> [CCRProvider]
    func presets() async throws -> [CCRProviderPreset]
    func accounts(provider: String, forceRefresh: Bool) async throws -> [CCRAccount]
    func saveProvider(_ draft: CCRProviderDraft) async throws -> CCRProvider
    func deleteProvider(id: String) async throws
    func setModelEnabled(providerID: String, model: String, enabled: Bool) async throws
    func setCredentialEnabled(providerID: String, credentialID: String, enabled: Bool) async throws
    func probeProvider(baseURL: String, apiKey: String?) async throws -> CCRProbeResult
}
