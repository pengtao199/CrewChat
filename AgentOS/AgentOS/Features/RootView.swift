import SwiftUI

struct RootView: View {
    @Environment(AppEnvironment.self) private var environment

    var body: some View {
        NavigationSplitView {
            List {
            }
            .overlay {
                ContentUnavailableView(
                    "sidebar.empty.title",
                    systemImage: "bubble.left.and.bubble.right",
                    description: Text("sidebar.empty.description")
                )
            }
            .navigationTitle("sidebar.sessions")
            .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            ConnectionView(model: environment.connection)
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                SettingsLink {
                    Label("settings.open", systemImage: "gearshape")
                }
            }
        }
        .task(id: environment.connection.isConnected) {
            guard environment.connection.isConnected else { return }
            await environment.coreSettings.applyDefaultsIfNeeded()
            await environment.coreSettings.load()
        }
    }
}
