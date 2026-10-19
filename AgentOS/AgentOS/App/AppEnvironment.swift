import SwiftUI

@MainActor
@Observable
final class AppEnvironment {
    let supervisor: CoreSupervisor
    let connection: ConnectionViewModel
    let coreSettings: CoreSettingsViewModel
    let models: ModelsSettingsViewModel
    let roster: AgentsSettingsViewModel
    let engines: EnginesSettingsViewModel
    var mode: AppMode = .workbench

    init() {
        let supervisor = CoreSupervisor()
        let provider = CoreClientProvider(
            endpoint: { [weak supervisor] in supervisor?.currentEndpoint() }
        )
        let enginesClient = CoreEnginesClient(provider: provider)
        let modelsClient = CoreModelsClient(provider: provider)
        let rosterClient = CoreRosterClient(provider: provider)

        self.supervisor = supervisor
        self.connection = ConnectionViewModel(lifecycle: supervisor)
        self.coreSettings = CoreSettingsViewModel(
            settings: CoreSettingsClient(provider: provider),
            lifecycle: supervisor
        )
        self.models = ModelsSettingsViewModel(models: modelsClient, engines: enginesClient)
        self.roster = AgentsSettingsViewModel(roster: rosterClient, engines: enginesClient)
        self.engines = EnginesSettingsViewModel(engines: enginesClient)
    }
}
