import SwiftUI

@MainActor
@Observable
final class AppEnvironment {
    let supervisor: CoreSupervisor
    let connection: ConnectionViewModel

    init() {
        let supervisor = CoreSupervisor()
        self.supervisor = supervisor
        self.connection = ConnectionViewModel(lifecycle: supervisor)
    }
}
