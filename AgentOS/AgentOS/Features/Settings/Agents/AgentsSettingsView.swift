import SwiftUI

struct AgentsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var isCreating = false
    @State private var role = AgentRole.empty
    @State private var savedRole: AgentRole?
    @State private var draftBaseline = AgentRole.empty
    @State private var isSaving = false
    @State private var saveError: String?
    @State private var saveTask: Task<Void, Never>?
    @State private var pendingDelete: AgentRole?
    @State private var soulExpanded = false
    @State private var rulesExpanded = false
    @State private var editingDocument: AgentDocument?

    private var model: AgentsSettingsViewModel { environment.roster }

    var body: some View {
        MasterDetailLayout {
            sidebar
        } right: {
            detail
        }
        .navigationTitle("settings.agents")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    Task { await createAgent() }
                } label: {
                    Label("common.add", systemImage: "plus")
                }
                .help("settings.agents.add")

                Button(role: .destructive) {
                    if let selected = model.agents.first(where: { $0.id == selection }) {
                        pendingDelete = selected
                    }
                } label: {
                    Label("common.delete", systemImage: "trash")
                }
                .disabled(selection == nil || isCreating || isSaving)
                .help("common.delete")
            }
        }
        .onChange(of: role) { _, _ in
            scheduleAutoSave()
        }
        .onChange(of: role.agentType) { _, _ in
            Task { await reloadForDraft() }
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
        }
        .onDisappear {
            saveTask?.cancel()
            Task { await saveDraft() }
        }
        .sheet(item: $editingDocument) { document in
            AgentDocumentSheet(
                title: document.titleKey,
                text: document == .soul ? $role.soul : $role.rules,
                onDone: {
                    saveTask?.cancel()
                    Task { await saveDraft() }
                }
            )
        }
        .alert(
            "settings.agents.delete.title",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }
            ),
            presenting: pendingDelete
        ) { agent in
            Button("common.delete", role: .destructive) {
                Task { await performDelete(agent) }
            }
            Button("common.cancel", role: .cancel) {
                pendingDelete = nil
            }
        } message: { agent in
            Text(String(format: String(localized: "settings.agents.delete.message"), agent.name))
        }
    }

    private var sidebar: some View {
        List(model.agents, selection: selectionBinding) { agent in
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Text(agent.name)
                    if let engine = model.engines.first(where: { $0.agentType == agent.agentType }),
                       !engine.available {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .font(.caption2)
                            .foregroundStyle(.orange)
                            .help("settings.engines.missing")
                    }
                }
                Text(subtitle(agent))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .tag(agent.id)
            .contextMenu {
                Button("settings.agents.copy") {
                    Task { await duplicate(agent) }
                }
                Divider()
                Button("common.delete", role: .destructive) {
                    pendingDelete = agent
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
                Button("settings.agents.retry") {
                    Task { await model.load() }
                }
            }
        } else if isCreating || selection != nil {
            editor
        } else if model.agents.isEmpty {
            ContentUnavailableView {
                Label("settings.agents.empty.title", systemImage: "person.2")
            } description: {
                Text("settings.agents.empty.description")
            } actions: {
                Button("common.add") {
                    Task { await createAgent() }
                }
            }
        } else {
            SelectItemPlaceholder()
        }
    }

    private var editor: some View {
        Form {
            if let saveError {
                Section {
                    Text(saveError)
                        .foregroundStyle(.red)
                }
            }

            Section("settings.agents.identity") {
                TextField("settings.agents.name", text: $role.name)
            }

            Section("settings.agents.runtime") {
                AgentRuntimeFields(
                    role: $role,
                    engines: model.engines,
                    enginesError: model.enginesError,
                    face: model.face,
                    faceError: model.faceError,
                    isLoadingFace: model.isLoadingFace,
                    onRetryFace: reloadFace
                )
            }

            Section("settings.agents.persona") {
                AgentPersonaFields(
                    soul: $role.soul,
                    rules: $role.rules,
                    soulExpanded: $soulExpanded,
                    rulesExpanded: $rulesExpanded,
                    onEdit: { editingDocument = $0 }
                )
            }

            Section("settings.agents.capabilities") {
                AgentCapabilitiesFields(
                    skills: $role.skills,
                    mcp: $role.mcp,
                    skillOptions: model.skills,
                    skillError: model.skillsError,
                    mcpOptions: model.mcpServers,
                    mcpError: model.mcpError,
                    isLoading: model.isLoadingCapabilities,
                    onRetry: reloadCapabilities
                )
            }

            Section("settings.agents.permissions") {
                AgentPermissionsFields(
                    permissionMode: $role.permissionMode,
                    autoApprove: $role.autoApprove
                )
            }
        }
        .formStyle(.grouped)
    }

    private func subtitle(_ agent: AgentRole) -> String {
        var parts = [model.engineName(for: agent.agentType)]
        if !agent.model.isEmpty {
            parts.append(agent.model)
        }
        return parts.joined(separator: " · ")
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
        role != (savedRole ?? draftBaseline)
    }

    private var isValidDraft: Bool {
        !role.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !role.agentType.isEmpty
    }

    private func scheduleAutoSave() {
        guard isDirty, isValidDraft, !isSaving else { return }

        saveTask?.cancel()
        saveTask = Task {
            try? await Task.sleep(for: .milliseconds(500))
            guard !Task.isCancelled else { return }
            await saveDraft()
        }
    }

    private func saveDraft() async {
        guard isDirty, isValidDraft, !isSaving else { return }

        isSaving = true
        saveError = nil
        let snapshot = role

        do {
            let saved = try await model.save(snapshot)
            savedRole = saved
            draftBaseline = saved

            if isCreating {
                isCreating = false
                selection = saved.id
            }

            if role == snapshot {
                role = saved
            } else if role.id.isEmpty {
                role.id = saved.id
            }
        } catch {
            saveError = error.localizedDescription
        }

        isSaving = false

        if isDirty, isValidDraft {
            scheduleAutoSave()
        }
    }

    private func switchSelection(to id: String?) async {
        guard id != selection else { return }

        saveTask?.cancel()
        await waitForSave()
        await saveDraft()

        isCreating = false
        selection = id

        if let id, let agent = model.agents.first(where: { $0.id == id }) {
            role = agent
            savedRole = agent
            draftBaseline = agent
            saveError = nil
        } else {
            role = .empty
            savedRole = nil
            draftBaseline = .empty
        }
    }

    private func createAgent() async {
        saveTask?.cancel()
        await waitForSave()
        await saveDraft()

        var draft = AgentRole.empty
        draft.agentType = model.engines.first(where: \.available)?.agentType ?? ""

        isCreating = true
        selection = nil
        role = draft
        draftBaseline = draft
        savedRole = nil
        saveError = nil
    }

    private func duplicate(_ agent: AgentRole) async {
        saveTask?.cancel()
        await waitForSave()
        await saveDraft()

        var copy = agent
        copy.id = ""
        copy.name = String(format: String(localized: "settings.agents.copyName"), agent.name)

        isCreating = true
        selection = nil
        role = copy
        draftBaseline = .empty
        savedRole = nil
        saveError = nil
        scheduleAutoSave()
    }

    private func reloadForDraft() async {
        await model.loadFace(agentType: role.agentType)
        await model.loadCapabilities(agentType: role.agentType)
    }

    private func reloadFace() {
        Task { await model.loadFace(agentType: role.agentType) }
    }

    private func reloadCapabilities() {
        Task { await model.loadCapabilities(agentType: role.agentType) }
    }

    private func waitForSave() async {
        while isSaving {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private func selectFirstIfNeeded() {
        guard selection == nil, !isCreating, let first = model.agents.first else { return }
        selection = first.id
        role = first
        savedRole = first
        draftBaseline = first
    }

    private func performDelete(_ agent: AgentRole) async {
        pendingDelete = nil
        saveTask?.cancel()
        await waitForSave()

        do {
            try await model.delete(agent.id)

            if selection == agent.id {
                selection = nil
                isCreating = false
                role = .empty
                savedRole = nil
                draftBaseline = .empty
                selectFirstIfNeeded()
            }
        } catch {
            saveError = error.localizedDescription
        }
    }
}
