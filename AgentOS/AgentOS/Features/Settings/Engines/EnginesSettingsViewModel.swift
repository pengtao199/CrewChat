import SwiftUI

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

    func setEnabled(_ engine: Engine, enabled: Bool) async {
        do {
            try await client.setEngineEnabled(engine.agentType, enabled: enabled)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }
}
