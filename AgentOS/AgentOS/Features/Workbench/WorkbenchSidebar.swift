import AppKit
import SwiftUI

struct WorkbenchSidebar: View {
    @Environment(AppEnvironment.self) private var environment
    @Binding var selection: Int?
    @State private var expanded: Set<Int> = []
    @State private var hoveredWorkspace: Int?
    @State private var hoveredConversation: Int?
    @State private var archiveTarget: Workspace?

    private var model: WorkbenchViewModel { environment.workbench }

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    ForEach(model.workspaces) { workspace in
                        workspaceRow(workspace)
                        if expanded.contains(workspace.id) {
                            ForEach(model.conversations(in: workspace.id)) { conversation in
                                conversationRow(conversation)
                            }
                        }
                    }
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
            }
            .overlay {
                if model.workspaces.isEmpty, !model.isLoading {
                    ContentUnavailableView(
                        "workbench.empty.title",
                        systemImage: "folder.badge.plus",
                        description: Text("workbench.empty.description")
                    )
                }
            }
            Divider()
            settingsRow
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .confirmationDialog(
            "workbench.archiveChats.confirm",
            isPresented: Binding(
                get: { archiveTarget != nil },
                set: { if !$0 { archiveTarget = nil } }
            ),
            presenting: archiveTarget
        ) { workspace in
            Button("workbench.archiveChats") {
                Task {
                    await model.archiveAll(in: workspace.id)
                    archiveTarget = nil
                }
            }
            Button("common.cancel", role: .cancel) {
                archiveTarget = nil
            }
        } message: { _ in
            Text("workbench.archiveChats.message")
        }
        .task {
            await model.load()
            await model.loadEngines()
        }
    }

    private var header: some View {
        HStack(spacing: 6) {
            Text("workbench.title")
                .font(.headline)
            Spacer()
            Menu {
                Section {
                    Button("workbench.group.project") {}
                        .disabled(true)
                    Button("workbench.group.list") {}
                        .disabled(true)
                } header: {
                    Text("workbench.arrange")
                }

                Section {
                    Button("workbench.sort.priority") {}
                        .disabled(true)
                    Button("workbench.sort.recent") {}
                        .disabled(true)
                    Button("workbench.sort.manual") {}
                        .disabled(true)
                } header: {
                    Text("workbench.sort")
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("workbench.more")

            Button {
                addWorkspace()
            } label: {
                Image(systemName: "plus")
            }
            .buttonStyle(.borderless)
            .help("workbench.addWorkspace")
        }
        .padding(.horizontal, 12)
        .padding(.top, 10)
        .padding(.bottom, 6)
    }

    private var settingsRow: some View {
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

    private func workspaceRow(_ workspace: Workspace) -> some View {
        let isExpanded = expanded.contains(workspace.id)
        let isHovered = hoveredWorkspace == workspace.id

        return HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .rotationEffect(.degrees(isExpanded ? 90 : 0))
                .frame(width: 10)

            Image(systemName: "folder")
                .foregroundStyle(.tint)

            Text(workspace.displayName)
                .lineLimit(1)

            Spacer(minLength: 4)

            if isHovered {
                Menu {
                    projectMenu(workspace)
                } label: {
                    Image(systemName: "ellipsis")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()

                Menu {
                    engineMenu(workspace)
                } label: {
                    Image(systemName: "square.and.pencil")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .help("workbench.newConversation")
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(rowBackground(selected: false, hovered: isHovered))
        .contentShape(Rectangle())
        .onHover { hovering in
            hoveredWorkspace = hovering ? workspace.id : nil
        }
        .onTapGesture {
            if isExpanded {
                expanded.remove(workspace.id)
            } else {
                expanded.insert(workspace.id)
            }
        }
        .contextMenu {
            projectMenu(workspace)
        }
    }

    private func conversationRow(_ conversation: Conversation) -> some View {
        let isSelected = selection == conversation.id
        let isHovered = hoveredConversation == conversation.id

        return HStack(spacing: 6) {
            Text(conversation.displayTitle)
                .lineLimit(1)
                .foregroundStyle(isSelected ? .primary : .secondary)

            Spacer(minLength: 4)

            if isHovered || isSelected {
                Button {
                } label: {
                    Image(systemName: "pin")
                }
                .buttonStyle(.borderless)
                .disabled(true)
                .help("workbench.pin")

                Button {
                    Task {
                        await model.archive(conversation.id)
                        if selection == conversation.id {
                            selection = nil
                        }
                    }
                } label: {
                    Image(systemName: "archivebox")
                }
                .buttonStyle(.borderless)
                .help("workbench.archive")
            }
        }
        .padding(.leading, 34)
        .padding(.trailing, 8)
        .padding(.vertical, 5)
        .background(rowBackground(selected: isSelected, hovered: isHovered))
        .contentShape(Rectangle())
        .onHover { hovering in
            hoveredConversation = hovering ? conversation.id : nil
        }
        .onTapGesture {
            selection = conversation.id
        }
        .contextMenu {
            Button("workbench.pin") {}
                .disabled(true)
            Button("workbench.archive") {
                Task {
                    await model.archive(conversation.id)
                    if selection == conversation.id {
                        selection = nil
                    }
                }
            }
        }
        .help("\(conversation.agentType) · \(conversation.updatedLabel)")
    }

    @ViewBuilder
    private func projectMenu(_ workspace: Workspace) -> some View {
        Button("workbench.pinProject") {}
            .disabled(true)
        Button("workbench.revealInFinder") {
            NSWorkspace.shared.activateFileViewerSelecting([
                URL(fileURLWithPath: workspace.path)
            ])
        }
        Button("workbench.worktree") {}
            .disabled(true)
        Button("workbench.editProject") {}
            .disabled(true)
        Divider()
        Button("workbench.archiveChats") {
            archiveTarget = workspace
        }
        Button("workbench.removeWorkspace", role: .destructive) {
            Task { await model.removeWorkspace(workspace.id) }
        }
    }

    @ViewBuilder
    private func engineMenu(_ workspace: Workspace) -> some View {
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
    }

    private func rowBackground(selected: Bool, hovered: Bool) -> some View {
        RoundedRectangle(cornerRadius: 6)
            .fill(selected ? Color.gray.opacity(0.22) : hovered ? Color.gray.opacity(0.12) : Color.clear)
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
