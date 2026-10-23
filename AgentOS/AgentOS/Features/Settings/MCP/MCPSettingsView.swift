import SwiftUI

struct MCPSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var query = ""
    @State private var selection: String?
    @State private var isCreating = false
    @State private var draft = MCPServerDraft.empty
    @State private var baseline = MCPServerDraft.empty
    @State private var isSaving = false
    @State private var saveError: String?
    @State private var saveTask: Task<Void, Never>?
    @State private var pendingDelete: MCPServer?
    @State private var advancedExpanded = false
    @State private var warnings: [String] = []

    private var model: MCPSettingsViewModel { environment.mcp }

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
                    Task { await startCreate() }
                } label: {
                    Label("settings.mcp.add", systemImage: "plus")
                }
                .help("settings.mcp.add")

                Button(role: .destructive) {
                    if let selectedServer {
                        pendingDelete = selectedServer
                    }
                } label: {
                    Label("common.delete", systemImage: "trash")
                }
                .disabled(selectedServer == nil || isCreating || isSaving)
                .help("common.delete")

                Button {
                    Task { await refresh() }
                } label: {
                    Label("settings.mcp.refresh", systemImage: "arrow.clockwise")
                }
                .disabled(model.isLoading || isSaving)
                .help("settings.mcp.refresh")
            }
        }
        .onChange(of: draft) { _, _ in
            scheduleAutoSave()
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
        }
        .onDisappear {
            saveTask?.cancel()
            Task { await saveDraft() }
        }
        .alert(
            "settings.mcp.delete.title",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }
            ),
            presenting: pendingDelete
        ) { server in
            Button("common.delete", role: .destructive) {
                Task { await performDelete(server) }
            }
            Button("common.cancel", role: .cancel) {
                pendingDelete = nil
            }
        } message: { server in
            Text(deleteMessage(for: server))
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

            Divider()

            if model.isLoading, model.servers.isEmpty {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if filteredServers.isEmpty {
                VStack(spacing: 8) {
                    Text(model.servers.isEmpty
                        ? "settings.mcp.empty.title"
                        : "settings.mcp.noMatches")
                        .font(.callout)
                        .foregroundStyle(.secondary)

                    if model.servers.isEmpty {
                        Button("settings.mcp.add") {
                            Task { await startCreate() }
                        }
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(20)
                Spacer()
            } else {
                List(filteredServers, selection: selectionBinding) { server in
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 5) {
                            Text(server.name)
                                .lineLimit(1)
                            if !server.enabled {
                                Text("settings.mcp.disabled")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Text(server.summary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    .tag(server.id)
                }
            }
        }
    }

    private var detail: some View {
        VStack(spacing: 0) {
            if !warnings.isEmpty {
                warningBanner
                Divider()
            }
            detailContent
        }
    }

    @ViewBuilder
    private var detailContent: some View {
        if let message = model.errorMessage, model.servers.isEmpty, !isCreating {
            ContentUnavailableView {
                Label("common.error", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                Button("settings.mcp.retry") {
                    Task { await refresh() }
                }
            }
        } else if isCreating || selection != nil {
            MCPDetailForm(
                draft: $draft,
                advancedExpanded: $advancedExpanded,
                isNew: isCreating,
                duplicateID: duplicateID,
                isSaving: isSaving,
                loadError: model.errorMessage,
                saveError: saveError
            )
        } else if model.servers.isEmpty {
            ContentUnavailableView {
                Label("settings.mcp.empty.title", systemImage: "puzzlepiece.extension")
            } description: {
                Text("settings.mcp.empty.description")
            } actions: {
                Button("settings.mcp.add") {
                    Task { await startCreate() }
                }
            }
        } else {
            SelectItemPlaceholder()
        }
    }

    private var warningBanner: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle")
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(warnings.enumerated()), id: \.offset) { _, warning in
                    Text(warning)
                        .font(.callout)
                        .foregroundStyle(.orange)
                }
            }
            Spacer()
            Button {
                warnings = []
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.borderless)
            .help("common.close")
        }
        .padding(10)
        .background(.orange.opacity(0.1))
    }

    private var filteredServers: [MCPServer] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return model.servers }
        return model.servers.filter {
            $0.name.localizedCaseInsensitiveContains(needle)
                || $0.id.localizedCaseInsensitiveContains(needle)
                || $0.summary.localizedCaseInsensitiveContains(needle)
                || $0.spec.description.localizedCaseInsensitiveContains(needle)
        }
    }

    private var selectedServer: MCPServer? {
        guard let selection else { return nil }
        return model.servers.first { $0.id == selection }
    }

    private var selectionBinding: Binding<String?> {
        Binding(
            get: { selection },
            set: { id in
                Task { await switchSelection(to: id) }
            }
        )
    }

    private var isDirty: Bool {
        draft != baseline
    }

    private var duplicateID: Bool {
        guard isCreating else { return false }
        let id = draft.id.trimmingCharacters(in: .whitespacesAndNewlines)
        return !id.isEmpty && model.contains(id: id)
    }

    private func scheduleAutoSave() {
        guard isDirty, draft.isValid, !duplicateID, !isSaving else { return }

        saveTask?.cancel()
        saveTask = Task {
            try? await Task.sleep(for: .milliseconds(500))
            guard !Task.isCancelled else { return }
            await saveDraft()
        }
    }

    private func saveDraft() async {
        guard isDirty, draft.isValid, !duplicateID, !isSaving else { return }
        guard let spec = draft.spec() else { return }

        isSaving = true
        saveError = nil
        let snapshot = draft
        let wasCreating = isCreating
        var didFail = false

        do {
            let result = try await model.save(id: spec.id, spec: spec)
            let savedDraft = MCPServerDraft(server: result.server)
            baseline = savedDraft
            warnings = result.warnings

            if wasCreating {
                isCreating = false
                selection = result.server.id
            }

            if draft == snapshot {
                draft = savedDraft
            }
        } catch {
            didFail = true
            saveError = error.localizedDescription
        }

        isSaving = false

        if !didFail, isDirty, draft.isValid, !duplicateID {
            scheduleAutoSave()
        }
    }

    private func startCreate() async {
        saveTask?.cancel()
        await waitForSave()
        await saveDraft()

        isCreating = true
        selection = nil
        draft = .empty
        baseline = .empty
        advancedExpanded = false
        saveError = nil
        warnings = []
    }

    private func switchSelection(to id: String?) async {
        guard id != selection else { return }

        saveTask?.cancel()
        await waitForSave()
        await saveDraft()

        isCreating = false
        selection = id
        advancedExpanded = false
        saveError = nil
        warnings = []

        if let id, let server = model.servers.first(where: { $0.id == id }) {
            draft = MCPServerDraft(server: server)
            baseline = draft
        } else {
            draft = .empty
            baseline = .empty
        }
    }

    private func refresh() async {
        saveTask?.cancel()
        await waitForSave()
        await saveDraft()
        await model.load()

        if let selection, let server = model.servers.first(where: { $0.id == selection }) {
            if !isDirty {
                draft = MCPServerDraft(server: server)
                baseline = draft
            }
        } else if selection != nil {
            selection = nil
            isCreating = false
            draft = .empty
            baseline = .empty
        }

        selectFirstIfNeeded()
    }

    private func waitForSave() async {
        while isSaving {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private func selectFirstIfNeeded() {
        guard selection == nil, !isCreating, let first = model.servers.first else { return }
        selection = first.id
        draft = MCPServerDraft(server: first)
        baseline = draft
    }

    private func performDelete(_ server: MCPServer) async {
        pendingDelete = nil
        saveTask?.cancel()
        await waitForSave()

        do {
            let result = try await model.remove(id: server.id)
            warnings = result.warnings

            if selection == server.id {
                selection = nil
                isCreating = false
                draft = .empty
                baseline = .empty
                saveError = nil
                selectFirstIfNeeded()
            }
        } catch {
            saveError = error.localizedDescription
        }
    }

    private func deleteMessage(for server: MCPServer) -> String {
        if model.isInUse(server.id) {
            return String(localized: "settings.mcp.delete.inUse")
        }
        return String(
            format: String(localized: "settings.mcp.delete.message"),
            server.name
        )
    }
}
