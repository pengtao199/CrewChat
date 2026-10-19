import SwiftUI

@MainActor
@Observable
final class AppEnvironment {
    let supervisor: CoreSupervisor
    let connection: ConnectionViewModel
    let coreSettings: CoreSettingsViewModel

    init() {
        let supervisor = CoreSupervisor()
        let settingsClient = CoreSettingsClient(
            endpointProvider: { [weak supervisor] in supervisor?.currentEndpoint() }
        )

        self.supervisor = supervisor
        self.connection = ConnectionViewModel(lifecycle: supervisor)
        self.coreSettings = CoreSettingsViewModel(settings: settingsClient, lifecycle: supervisor)
    }
}
