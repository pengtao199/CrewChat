import SwiftUI

@MainActor
@Observable
final class ModelsSettingsViewModel {
    private(set) var providers: [ModelProvider] = []
    private(set) var presets: [ModelProviderPreset] = []
    private(set) var health: ModelHealth?
    private(set) var isLoading = false
    private(set) var isStarting = false
    private(set) var errorMessage: String?

    private let model: CoreModel

    init(model: CoreModel) {
        self.model = model
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            health = try await model.health()
            providers = try await model.providers()
            presets = (try? await model.presets()) ?? []
        } catch {
            health = nil
            providers = []
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func startModel() async {
        isStarting = true
        errorMessage = nil

        do {
            try await model.start()
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }

        isStarting = false
    }

    func accounts(for provider: String, forceRefresh: Bool) async -> Result<[ModelAccount], Error> {
        do {
            return .success(try await model.accounts(provider: provider, forceRefresh: forceRefresh))
        } catch {
            return .failure(error)
        }
    }

    func saveProvider(_ draft: ModelProviderDraft) async throws -> ModelProvider {
        let saved = try await model.saveProvider(draft)
        await load()
        return saved
    }

    func deleteProvider(id: String) async throws {
        try await model.deleteProvider(id: id)
        await load()
    }

    func setModelEnabled(providerID: String, modelID: String, enabled: Bool) async throws {
        try await model.setModelEnabled(providerID: providerID, model: modelID, enabled: enabled)
        await load()
    }

    func setCredentialEnabled(
        providerID: String,
        credentialID: String,
        enabled: Bool
    ) async throws {
        try await model.setCredentialEnabled(
            providerID: providerID,
            credentialID: credentialID,
            enabled: enabled
        )
        await load()
    }

    func probeProvider(baseURL: String, apiKey: String?) async throws -> ModelProbeResult {
        try await model.probeProvider(baseURL: baseURL, apiKey: apiKey)
    }
}
