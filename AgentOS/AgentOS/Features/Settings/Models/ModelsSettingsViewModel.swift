import SwiftUI

@MainActor
@Observable
final class ModelsSettingsViewModel {
    private(set) var providers: [CCRProvider] = []
    private(set) var health: CCRHealth?
    private(set) var isLoading = false
    private(set) var isStarting = false
    private(set) var errorMessage: String?

    private let ccr: CoreCCR

    init(ccr: CoreCCR) {
        self.ccr = ccr
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            health = try await ccr.health()
            providers = try await ccr.providers()
        } catch {
            health = nil
            providers = []
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func startCCR() async {
        isStarting = true
        errorMessage = nil

        do {
            try await ccr.start()
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }

        isStarting = false
    }

    func accounts(for provider: String, forceRefresh: Bool) async -> Result<[CCRAccount], Error> {
        do {
            return .success(try await ccr.accounts(provider: provider, forceRefresh: forceRefresh))
        } catch {
            return .failure(error)
        }
    }

    func saveProvider(_ draft: CCRProviderDraft) async throws -> CCRProvider {
        let saved = try await ccr.saveProvider(draft)
        await load()
        return saved
    }

    func deleteProvider(id: String) async throws {
        try await ccr.deleteProvider(id: id)
        await load()
    }

    func probeProvider(baseURL: String, apiKey: String?) async throws -> CCRProbeResult {
        try await ccr.probeProvider(baseURL: baseURL, apiKey: apiKey)
    }
}
