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
        .commands {
            CommandGroup(replacing: .appSettings) {
                Button("settings.open") {
                    environment.mode = .settings
                }
                .keyboardShortcut(",", modifiers: .command)
            }
        }
    }
}
