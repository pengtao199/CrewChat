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
                engineField

                if let enginesError = model.enginesError {
                    Text(enginesError)
                        .font(.caption)
                        .foregroundStyle(.red)
                }

                modelFields
            }

            Section("settings.agents.persona") {
                personaGroup(
                    title: "settings.agents.soul",
                    text: role.soul,
                    expanded: $soulExpanded,
                    document: .soul
                )
                personaGroup(
                    title: "settings.agents.rules",
                    text: role.rules,
                    expanded: $rulesExpanded,
                    document: .rules
                )
            }

            Section("settings.agents.capabilities") {
                CapabilityMultiSelect(
                    title: "settings.agents.skills",
                    options: model.skills,
                    knownIDs: Set(model.skills.map(\.id)),
                    selection: $role.skills,
                    isLoading: model.isLoadingCapabilities,
                    error: model.skillsError,
                    searchPrompt: "settings.agents.searchSkills",
                    isEnabled: true,
                    onRetry: reloadCapabilities
                )

                CapabilityMultiSelect(
                    title: "settings.agents.mcp",
                    options: model.mcpServers,
                    knownIDs: Set(model.mcpServers.map(\.id)).union(role.mcp),
                    selection: $role.mcp,
                    isLoading: model.isLoadingCapabilities,
                    error: model.mcpError,
                    searchPrompt: "settings.agents.searchMCP",
                    isEnabled: false,
                    onRetry: reloadCapabilities
                )
            }

            Section("settings.agents.permissions") {
                Picker("settings.agents.permissionMode", selection: $role.permissionMode) {
                    Text("settings.agents.permissionMode.default").tag("")
                    ForEach(permissionModes, id: \.self) { mode in
                        Text(modeLabel(mode)).tag(mode)
                    }
                    if !role.permissionMode.isEmpty, !permissionModes.contains(role.permissionMode) {
                        Text(role.permissionMode).tag(role.permissionMode)
                    }
                }

                Toggle("settings.agents.autoApprove", isOn: $role.autoApprove)

                if role.autoApprove {
                    Text("settings.agents.autoApproveHint")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            }
        }
        .formStyle(.grouped)
    }

    @ViewBuilder
    private var engineField: some View {
        if runnableEngines.isEmpty {
            Text("settings.agents.engine.noneAvailable")
                .font(.callout)
                .foregroundStyle(.secondary)
        } else {
            Picker("settings.agents.engine", selection: $role.agentType) {
                if role.agentType.isEmpty {
                    Text("settings.agents.engine.none").tag("")
                }
                ForEach(runnableEngines) { engine in
                    Text(engine.name).tag(engine.agentType)
                }
            }

            if !role.agentType.isEmpty,
               !runnableEngines.contains(where: { $0.agentType == role.agentType }) {
                Text("settings.agents.engine.unavailableCurrent")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
    }

    @ViewBuilder
    private var modelFields: some View {
        if model.isLoadingFace {
            ProgressView().controlSize(.small)
        } else if let faceError = model.faceError {
            errorLine(faceError, retry: reloadFace)
        } else if let face = model.face {
            if face.supported {
                providerField
                modelField

                if let hint = faceHint {
                    Text(hint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else if let hint = faceHint {
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var providerField: some View {
        Picker("settings.agents.provider", selection: providerBinding) {
            if role.modelProvider.isEmpty {
                Text("settings.agents.model.default").tag("")
            }
            ForEach(providerChoices) { choice in
                Text(choice.name).tag(choice.id)
            }
        }
        .disabled(providerChoices.isEmpty)
    }

    private var modelField: some View {
        VStack(alignment: .leading, spacing: 4) {
            Picker("settings.agents.model", selection: $role.model) {
                Text("settings.agents.model.default").tag("")
                ForEach(modelOptions, id: \.self) { name in
                    Text(name).tag(name)
                }
            }
            .disabled(modelOptions.isEmpty)

            if let face = model.face, face.supported, !role.model.isEmpty, !modelOptions.contains(role.model) {
                HStack(spacing: 6) {
                    Label("settings.agents.modelInvalid", systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                    Button("settings.agents.modelClear") {
                        role.model = ""
                    }
                    .buttonStyle(.link)
                    .font(.caption)
                }
            }
        }
    }

    private func personaGroup(
        title: LocalizedStringKey,
        text: String,
        expanded: Binding<Bool>,
        document: AgentDocument
    ) -> some View {
        DisclosureGroup(isExpanded: expanded) {
            if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Text("settings.agents.persona.empty")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ScrollView {
                    StructuredText(markdown: text)
                        .textual.structuredTextStyle(.gitHub)
                        .textual.textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 80, maxHeight: 180)
            }
        } label: {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)

                    if let summary = firstLine(text) {
                        Text(summary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }

                Spacer()

                Button("settings.agents.edit") {
                    editingDocument = document
                }
                .buttonStyle(.borderless)
                .font(.caption)
            }
        }
    }

    private var providerChoices: [ProviderChoice] {
        var seen = Set<String>()
        var result: [ProviderChoice] = []
        for model in model.face?.models ?? [] {
            let id = model.providerID.isEmpty ? model.provider : model.providerID
            guard seen.insert(id).inserted else { continue }
            result.append(ProviderChoice(id: id, name: model.provider))
        }
        return result
    }

    private var resolvedProviderID: String {
        if providerChoices.contains(where: { $0.id == role.modelProvider }) {
            return role.modelProvider
        }
        if let choice = providerChoices.first(where: { $0.name == role.modelProvider }) {
            return choice.id
        }
        return role.modelProvider
    }

    private var providerBinding: Binding<String> {
        Binding(
            get: { resolvedProviderID },
            set: { providerID in
                role.modelProvider = providerID
                if !modelOptions(for: providerID).contains(role.model) {
                    role.model = ""
                }
            }
        )
    }

    private var modelOptions: [String] {
        modelOptions(for: resolvedProviderID)
    }

    private func modelOptions(for providerID: String) -> [String] {
        (model.face?.models ?? [])
            .filter { ($0.providerID.isEmpty ? $0.provider : $0.providerID) == providerID }
            .map(\.model)
    }

    private var faceHint: String? {
        guard let face = model.face else { return nil }
        if !face.supported {
            return face.reason ?? String(localized: "settings.agents.modelUnsupported")
        }
        if face.models.isEmpty {
            return face.reason ?? String(localized: "settings.agents.modelUnsupported")
        }
        return nil
    }

    private var permissionModes: [String] {
        ["default", "acceptEdits", "auto", "dontAsk", "bypassPermissions", "plan"]
    }

    private func modeLabel(_ mode: String) -> LocalizedStringKey {
        switch mode {
        case "default": "settings.agents.permissionMode.default"
        case "acceptEdits": "settings.agents.permissionMode.acceptEdits"
        case "auto": "settings.agents.permissionMode.auto"
        case "dontAsk": "settings.agents.permissionMode.dontAsk"
        case "bypassPermissions": "settings.agents.permissionMode.bypass"
        case "plan": "settings.agents.permissionMode.plan"
        default: LocalizedStringKey(mode)
        }
    }

    private var runnableEngines: [Engine] {
        model.engines.filter(\.available)
    }

    private func errorLine(_ message: String, retry: @escaping () -> Void) -> some View {
        HStack(spacing: 6) {
            Text(message)
                .font(.caption)
                .foregroundStyle(.red)
            Button("settings.agents.retry", action: retry)
                .buttonStyle(.link)
                .font(.caption)
        }
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

    private func firstLine(_ text: String) -> String? {
        text.split(separator: "\n", omittingEmptySubsequences: false)
            .lazy
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty }
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
        draft.agentType = runnableEngines.first?.agentType ?? ""

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
        normalizeModelSelection()
    }

    private func reloadFace() {
        Task {
            await model.loadFace(agentType: role.agentType)
            normalizeModelSelection()
        }
    }

    private func reloadCapabilities() {
        Task { await model.loadCapabilities(agentType: role.agentType) }
    }

    private func normalizeModelSelection() {
        guard let face = model.face else { return }

        if !face.supported {
            role.modelProvider = ""
            role.model = ""
            return
        }

        let providers = providerChoices
        if !providers.contains(where: { $0.id == resolvedProviderID }) {
            role.modelProvider = providers.first?.id ?? ""
        } else if role.modelProvider != resolvedProviderID {
            role.modelProvider = resolvedProviderID
        }

        if !role.model.isEmpty, !modelOptions.contains(role.model) {
            role.model = ""
        }
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

private enum AgentDocument: String, Identifiable {
    case soul
    case rules

    var id: String { rawValue }

    var titleKey: LocalizedStringKey {
        switch self {
        case .soul: "settings.agents.soul"
        case .rules: "settings.agents.rules"
        }
    }
}

private struct ProviderChoice: Identifiable, Hashable {
    let id: String
    let name: String
}
