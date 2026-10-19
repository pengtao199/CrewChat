import SwiftUI

@main
struct AgentOSApp: App {
    @State private var environment = AppEnvironment()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(environment)
        }
        .defaultSize(width: 1080, height: 720)
    }
}
