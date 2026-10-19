import SwiftUI

@MainActor
@Observable
final class ModelsSettingsViewModel {
    private(set) var providers: [ModelProvider] = []
    private(set) var engines: [Engine] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    private let models: CoreModels
    private let enginesClient: CoreEngines

    init(models: CoreModels, engines: CoreEngines) {
        self.models = models
        self.enginesClient = engines
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            async let providers = models.modelProviders()
            async let engines = enginesClient.engines()
            self.providers = try await providers
            self.engines = try await engines
        } catch {
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func create(_ draft: ModelProviderDraft) async throws -> ModelProvider {
        let created = try await models.createModelProvider(draft)
        await load()
        return created
    }

    func update(id: Int, _ draft: ModelProviderDraft) async throws {
        try await models.updateModelProvider(id: id, draft)
        await load()
    }

    func delete(_ id: Int) async {
        do {
            try await models.deleteModelProvider(id: id)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
