import Foundation

@MainActor
@Observable
final class EnginesSettingsViewModel {
    private(set) var engines: [Engine] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?

    private let client: CoreEngines

    init(engines: CoreEngines) {
        self.client = engines
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            engines = try await client.engines()
        } catch {
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func update(_ engine: Engine, enabled: Bool, env: [String: String]) async throws {
        try await client.setPreferences(
            EnginePreferences(
                agentType: engine.agentType,
                enabled: enabled,
                env: env
            )
        )
        await load()
    }
}
