import SwiftUI

@main
struct AgentOSApp: App {
    @State private var environment = AppEnvironment()
    @AppStorage("appearance") private var appearance = Appearance.system

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(environment)
                .preferredColorScheme(appearance.colorScheme)
        }
        .defaultSize(width: 1080, height: 720)

        Settings {
            SettingsView()
                .environment(environment)
                .preferredColorScheme(appearance.colorScheme)
        }
    }
}
