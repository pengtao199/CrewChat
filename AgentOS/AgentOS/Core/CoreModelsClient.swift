import Foundation

final class CoreModelsClient: CoreModels, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func modelProviders() async throws -> [ModelProvider] {
        let rows: [CoreModelProvider] = try await provider.client().send("list_model_providers", body: EmptyBody())
        return rows.map(\.domain)
    }

    func createModelProvider(_ draft: ModelProviderDraft) async throws -> ModelProvider {
        let created: CoreModelProvider = try await provider.client().send(
            "create_model_provider",
            body: CoreCreateModelProvider(draft)
        )
        return created.domain
    }

    func updateModelProvider(id: Int, _ draft: ModelProviderDraft) async throws {
        try await provider.client().sendIgnoring("update_model_provider", body: CoreUpdateModelProvider(id: id, draft: draft))
    }

    func deleteModelProvider(id: Int) async throws {
        try await provider.client().sendIgnoring("delete_model_provider", body: CoreIdBody(id: id))
    }
}

struct CoreIdBody: Encodable {
    let id: Int
}
