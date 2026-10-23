import SwiftUI

struct RootView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var settingsPage: SettingsPage? = .general
    @State private var pendingExit: SettingsExit?
    @State private var showExitAlert = false

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            detail
        }
        .onChange(of: environment.workbench.selectedConversationID) { _, value in
            if value != nil {
                environment.workbench.cancelDraft()
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
                        requestExit(.workbench)
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
        .alert(
            "settings.leave.title",
            isPresented: $showExitAlert,
            presenting: pendingExit
        ) { exit in
            Button("settings.leave.discard", role: .destructive) {
                performExit(exit)
            }
            Button("common.cancel", role: .cancel) {
                pendingExit = nil
            }
        } message: { _ in
            Text("settings.leave.message")
        }
    }

    private var settingsPageBinding: Binding<SettingsPage?> {
        Binding(
            get: { settingsPage },
            set: { page in
                guard page != settingsPage else { return }
                requestExit(.page(page))
            }
        )
    }

    private func requestExit(_ exit: SettingsExit) {
        if environment.roster.hasUnsavedChanges {
            pendingExit = exit
            showExitAlert = true
        } else {
            performExit(exit)
        }
    }

    private func performExit(_ exit: SettingsExit) {
        pendingExit = nil
        showExitAlert = false

        switch exit {
        case let .page(page):
            settingsPage = page
        case .workbench:
            environment.roster.hasUnsavedChanges = false
            environment.mode = .workbench
        }
    }

    @ViewBuilder
    private var sidebar: some View {
        switch environment.mode {
        case .workbench:
            WorkbenchSidebar()

        case .settings:
            List(SettingsPage.allCases, id: \.self, selection: settingsPageBinding) { page in
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

private enum SettingsExit {
    case page(SettingsPage?)
    case workbench
}
