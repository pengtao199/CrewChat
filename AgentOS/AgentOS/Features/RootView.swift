import SwiftUI

struct RootView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var settingsPage: SettingsPage? = .general

    var body: some View {
        @Bindable var workbench = environment.workbench

        return NavigationSplitView {
            sidebar(selection: $workbench.selectedConversationID)
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            detail
        }
        .onChange(of: workbench.selectedConversationID) { _, value in
            if value != nil {
                workbench.cancelDraft()
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                if environment.mode == .workbench, environment.workbench.isRunning {
                    Button {
                        Task { await environment.workbench.cancelRun() }
                    } label: {
                        Label("workbench.stop", systemImage: "stop.circle")
                    }
                }
            }
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
    private func sidebar(selection: Binding<Int?>) -> some View {
        switch environment.mode {
        case .workbench:
            WorkbenchSidebar(selection: selection)

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
            if !environment.connection.isConnected {
                ConnectionView(model: environment.connection)
            } else if let workspace = environment.workbench.draftFolder {
                DraftView(workspace: workspace)
            } else {
                ConversationPane(conversationID: environment.workbench.selectedConversationID)
            }

        case .settings:
            SettingsDetail(page: settingsPage)
        }
    }
}
