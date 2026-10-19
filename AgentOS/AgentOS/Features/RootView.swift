import SwiftUI

struct RootView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var settingsPage: SettingsPage? = .general
    @State private var selectedConversation: Int?

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            detail
        }
        .toolbar {
            ToolbarItem(placement: .navigation) {
                if environment.mode == .settings {
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
            WorkbenchSidebar(selection: $selectedConversation)

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
            if environment.connection.isConnected {
                ConversationPane(conversationID: selectedConversation)
            } else {
                ConnectionView(model: environment.connection)
            }

        case .settings:
            SettingsDetail(page: settingsPage)
        }
    }
}
