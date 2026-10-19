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
                            row(conversation)
                                .tag(conversation.id)
                        }
                    }
                } label: {
                    Label(workspace.displayName, systemImage: "folder")
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
            }
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
        .navigationTitle("workbench.title")
        .task {
            await model.load()
        }
    }

    private func row(_ conversation: Conversation) -> some View {
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
}
