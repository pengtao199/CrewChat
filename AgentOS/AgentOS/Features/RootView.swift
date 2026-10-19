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
    }
}
