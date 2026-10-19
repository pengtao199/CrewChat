import Foundation

protocol CoreModels: Sendable {
    func modelProviders() async throws -> [ModelProvider]
    func createModelProvider(_ draft: ModelProviderDraft) async throws
    func updateModelProvider(id: Int, _ draft: ModelProviderDraft) async throws
    func deleteModelProvider(id: Int) async throws
}
