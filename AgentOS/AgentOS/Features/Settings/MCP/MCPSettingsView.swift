import SwiftUI

struct MCPSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var query = ""
    @State private var selection: String?
    @State private var editor: EditorTarget?
    @State private var confirmDelete = false
    @State private var actionError: String?
    @State private var warningsExpanded = false

    private var model: MCPSettingsViewModel { environment.mcp }

    private var selected: MCPServer? {
        guard let selection else { return nil }
        return model.servers.first { $0.id == selection }
    }

    private var filteredServers: [MCPServer] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return model.servers }
        return model.servers.filter {
            $0.id.localizedCaseInsensitiveContains(needle)
                || $0.summary.localizedCaseInsensitiveContains(needle)
        }
    }

    var body: some View {
        MasterDetailLayout {
            sidebar
        } right: {
            detail
        }
        .navigationTitle("settings.mcp")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    editor = .create
                } label: {
                    Label("settings.mcp.add", systemImage: "plus")
                }

                Button {
                    if let selected {
                        editor = .edit(selected)
                    }
                } label: {
                    Label("common.edit", systemImage: "pencil")
                }
                .disabled(selected == nil)

                Button(role: .destructive) {
                    confirmDelete = true
                } label: {
                    Label("common.delete", systemImage: "trash")
                }
                .disabled(selected == nil)

                Button {
                    Task { await model.load() }
                } label: {
                    Label("settings.mcp.refresh", systemImage: "arrow.clockwise")
                }
                .disabled(model.isLoading)
            }
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
        }
        .sheet(item: $editor) { target in
            switch target {
            case .create:
                MCPServerEditorSheet(existing: nil, model: model) { saved in
                    if let saved {
                        selection = saved.id
                    }
                }
            case let .edit(server):
                MCPServerEditorSheet(existing: server, model: model) { saved in
                    if let saved {
                        selection = saved.id
                    } else {
                        selection = nil
                        selectFirstIfNeeded()
                    }
                }
            }
        }
        .alert(
            "settings.mcp.delete.title",
            isPresented: $confirmDelete,
            presenting: selected
        ) { server in
            Button("common.delete", role: .destructive) {
                Task { await remove(server) }
            }
            Button("common.cancel", role: .cancel) {}
        } message: { server in
            Text(String(format: String(localized: "settings.mcp.delete.message"), server.id))
        }
    }

    private var sidebar: some View {
        VStack(spacing: 0) {
            TextField(
                "",
                text: $query,
                prompt: Text("settings.mcp.search")
            )
            .labelsHidden()
            .textFieldStyle(.roundedBorder)
            .padding(10)

            if !model.warnings.isEmpty {
                DisclosureGroup(isExpanded: $warningsExpanded) {
                    ForEach(model.warnings) { warning in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(verbatim: appLabel(warning.app))
                                .font(.caption.weight(.semibold))
                            Text(warning.message)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 2)
                    }
                } label: {
                    Label(
                        String(
                            format: String(localized: "settings.mcp.warnings"),
                            model.warnings.count
                        ),
                        systemImage: "exclamationmark.triangle"
                    )
                    .font(.caption)
                    .foregroundStyle(.orange)
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 8)
            }

            Divider()

            if model.isLoading, model.servers.isEmpty {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if filteredServers.isEmpty {
                VStack(spacing: 8) {
                    Text("settings.mcp.empty.title")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Button("settings.mcp.add") {
                        editor = .create
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(20)
                Spacer()
            } else {
                List(filteredServers, selection: $selection) { server in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(server.id)
                        Text(server.summary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                        Text(String(format: String(localized: "settings.mcp.appCount"), server.apps.count))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    .tag(server.id)
                }
            }
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let message = model.errorMessage {
            ContentUnavailableView {
                Label("common.error", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                Button("settings.mcp.retry") {
                    Task { await model.load() }
                }
            }
        } else if let server = selected {
            serverDetail(server)
        } else if model.servers.isEmpty {
            ContentUnavailableView {
                Label("settings.mcp.empty.title", systemImage: "puzzlepiece.extension")
            } description: {
                Text("settings.mcp.empty.description")
            } actions: {
                Button("settings.mcp.add") {
                    editor = .create
                }
            }
        } else {
            SelectItemPlaceholder()
        }
    }

    private func serverDetail(_ server: MCPServer) -> some View {
        Form {
            if let actionError {
                Section {
                    Text(actionError)
                        .foregroundStyle(.red)
                }
            }

            Section("settings.mcp.basic") {
                LabeledContent("settings.mcp.id", value: server.id)
                LabeledContent("settings.mcp.type", value: server.type)
                LabeledContent("settings.mcp.apps") {
                    Text(verbatim: server.apps.isEmpty
                        ? String(localized: "settings.mcp.noApps")
                        : server.apps.map(appLabel).joined(separator: " · "))
                }
                LabeledContent("settings.mcp.summary") {
                    Text(server.summary)
                        .textSelection(.enabled)
                }
            }

            Section("settings.mcp.spec") {
                ScrollView {
                    Text(server.spec.prettyText)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                }
                .frame(minHeight: 160, maxHeight: 320)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(.separator)
                )
            }

            Section {
                HStack {
                    Button("common.edit") {
                        editor = .edit(server)
                    }
                    Button("common.delete", role: .destructive) {
                        confirmDelete = true
                    }
                    Spacer()
                }
            }
        }
        .formStyle(.grouped)
    }

    private func appLabel(_ app: String) -> String {
        NSLocalizedString(MCPAppCatalog.labelKey(for: app), comment: "")
    }

    private func selectFirstIfNeeded() {
        guard selection == nil, let first = model.servers.first else { return }
        selection = first.id
    }

    private func remove(_ server: MCPServer) async {
        actionError = nil
        do {
            try await model.remove(serverID: server.id)
            selection = nil
            selectFirstIfNeeded()
        } catch {
            actionError = error.localizedDescription
        }
    }
}

private enum EditorTarget: Identifiable {
    case create
    case edit(MCPServer)

    var id: String {
        switch self {
        case .create: "create"
        case let .edit(server): server.id
        }
    }
}
