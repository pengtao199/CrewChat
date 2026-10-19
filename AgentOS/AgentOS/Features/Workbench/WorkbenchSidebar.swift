import AppKit
import SwiftUI

struct WorkbenchSidebar: View {
    @Environment(AppEnvironment.self) private var environment
    @Binding var selection: Int?
    @State private var expanded: Set<Int> = []

    private var model: WorkbenchViewModel { environment.workbench }

    var body: some View {
        List(selection: $selection) {
            ForEach(model.workspaces) { workspace in
                DisclosureGroup(isExpanded: expansion(workspace.id)) {
                    let rows = model.conversations(in: workspace.id)

                    if rows.isEmpty {
                        Text("workbench.noConversations")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(rows) { conversation in
                            conversationRow(conversation)
                                .tag(conversation.id)
                        }
                    }

                    newConversationMenu(workspace)
                } label: {
                    workspaceLabel(workspace)
                }
            }
        }
        .listStyle(.sidebar)
        .overlay {
            if model.workspaces.isEmpty, !model.isLoading {
                ContentUnavailableView(
                    "workbench.empty.title",
                    systemImage: "folder.badge.plus",
                    description: Text("workbench.empty.description")
                )
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) { header }
        .safeAreaInset(edge: .bottom, spacing: 0) { footer }
        .task {
            await model.load()
            await model.loadEngines()
        }
    }

    private var header: some View {
        HStack {
            Text("workbench.title")
                .font(.headline)
            Spacer()
            Button {
                addWorkspace()
            } label: {
                Image(systemName: "plus")
            }
            .buttonStyle(.borderless)
            .help("workbench.addWorkspace")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var footer: some View {
        VStack(spacing: 0) {
            Divider()
            Button {
                environment.mode = .settings
            } label: {
                Label("settings.title", systemImage: "gearshape")
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
        }
        .background(.bar)
    }

    private func workspaceLabel(_ workspace: Workspace) -> some View {
        Label(workspace.displayName, systemImage: "folder")
            .lineLimit(1)
            .contextMenu {
                Button("workbench.revealInFinder") {
                    NSWorkspace.shared.activateFileViewerSelecting([
                        URL(fileURLWithPath: workspace.path)
                    ])
                }
                Divider()
                Button("workbench.removeWorkspace", role: .destructive) {
                    Task { await model.removeWorkspace(workspace.id) }
                }
            }
    }

    private func conversationRow(_ conversation: Conversation) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(conversation.displayTitle)
                .lineLimit(1)
            HStack(spacing: 6) {
                Text(conversation.agentType)
                Text(conversation.updatedLabel)
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }

    private func newConversationMenu(_ workspace: Workspace) -> some View {
        Menu {
            let enabled = model.engines.filter(\.enabled)
            if enabled.isEmpty {
                Text("workbench.noEngines")
            } else {
                ForEach(enabled) { engine in
                    Button(engine.name) {
                        Task { await createConversation(in: workspace, agentType: engine.agentType) }
                    }
                }
            }
        } label: {
            Label("workbench.newConversation", systemImage: "plus")
                .font(.callout)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
    }

    private func expansion(_ id: Int) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(id) },
            set: { isOn in
                if isOn {
                    expanded.insert(id)
                } else {
                    expanded.remove(id)
                }
            }
        )
    }

    private func addWorkspace() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = false
        panel.prompt = String(localized: "workbench.addWorkspace")

        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { await model.addWorkspace(path: url.path) }
    }

    private func createConversation(in workspace: Workspace, agentType: String) async {
        guard let id = await model.createConversation(in: workspace.id, agentType: agentType) else {
            return
        }
        expanded.insert(workspace.id)
        selection = id
    }
}
