import SwiftUI

struct RootView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var settingsPage: SettingsPage? = .general

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            detail
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                switch environment.mode {
                case .workbench:
                    Button {
                        environment.mode = .settings
                    } label: {
                        Label("settings.open", systemImage: "gearshape")
                    }
                case .settings:
                    Button {
                        environment.mode = .workbench
                    } label: {
                        Label("settings.back", systemImage: "chevron.left")
                    }
                }
            }
        }
        .task(id: environment.connection.isConnected) {
            guard environment.connection.isConnected else { return }
            await environment.coreSettings.applyDefaultsIfNeeded()
            await environment.coreSettings.load()
        }
    }

    @ViewBuilder
    private var sidebar: some View {
        switch environment.mode {
        case .workbench:
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

        case .settings:
            List(SettingsPage.allCases, id: \.self, selection: $settingsPage) { page in
                Label(page.titleKey, systemImage: page.symbol)
                    .tag(page)
            }
            .navigationTitle("settings.title")
            .onAppear {
                if settingsPage == nil {
                    settingsPage = .general
                }
            }
        }
    }

    @ViewBuilder
    private var detail: some View {
        switch environment.mode {
        case .workbench:
            ConnectionView(model: environment.connection)
        case .settings:
            SettingsDetail(page: settingsPage)
        }
    }
}
