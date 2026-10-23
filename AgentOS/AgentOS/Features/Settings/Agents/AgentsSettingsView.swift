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
    @State private var pendingChange: PendingChange?
    @State private var pendingDelete: AgentRole?

    private var model: AgentsSettingsViewModel { environment.roster }

    private enum PendingChange: Equatable {
        case select(String?)
        case create
        case duplicate(AgentRole)
    }

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
                    request(.create)
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
                .disabled(selection == nil || isCreating)
                .help("common.delete")
            }
        }
        .onChange(of: selection) { _, _ in
            loadSelection()
        }
        .onChange(of: role.agentType) { _, _ in
            Task { await reloadForDraft() }
        }
        .onChange(of: isDirty, initial: true) { _, dirty in
            model.hasUnsavedChanges = dirty
        }
        .onDisappear {
            model.hasUnsavedChanges = false
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
        }
        .alert(
            "settings.agents.discard.title",
            isPresented: Binding(
                get: { pendingChange != nil },
                set: { if !$0 { pendingChange = nil } }
            ),
            presenting: pendingChange
        ) { change in
            Button("settings.agents.discard", role: .destructive) {
                apply(change)
            }
            Button("common.cancel", role: .cancel) {
                pendingChange = nil
            }
        } message: { _ in
            Text("settings.agents.discard.message")
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
                    request(.duplicate(agent))
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
                    startCreating()
                }
            }
        } else {
            SelectItemPlaceholder()
        }
    }

    private var editor: some View {
        Form {
            Section("settings.agents.identity") {
                TextField("settings.agents.name", text: $role.name)
            }

            Section("settings.agents.runtime") {
                engineField

                if let engine = selectedEngine {
                    LabeledContent("settings.agents.engineStatus") {
                        Text(engineStatus(engine))
                            .foregroundStyle(engine.available ? .primary : .secondary)
                    }
                }

                if let enginesError = model.enginesError {
                    Text(enginesError)
                        .font(.caption)
                        .foregroundStyle(.red)
                }

                providerField
                modelField

                if let face = model.face {
                    LabeledContent("settings.agents.modelInjection") {
                        Text(
                            face.supported
                                ? LocalizedStringKey("settings.agents.modelInjection.supported")
                                : LocalizedStringKey("settings.agents.modelInjection.unsupported")
                        )
                        .foregroundStyle(face.supported ? .green : .orange)
                    }

                    if let hint = faceHint {
                        Text(hint)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Section("settings.agents.persona") {
                markdownPreview(
                    title: "settings.agents.soul",
                    text: role.soul
                )
                markdownPreview(
                    title: "settings.agents.rules",
                    text: role.rules
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
                    onRetry: reloadCapabilities
                )

                CapabilityMultiSelect(
                    title: "settings.agents.mcp",
                    options: model.mcpServers,
                    knownIDs: Set(model.mcpServers.map(\.id)),
                    selection: $role.mcp,
                    isLoading: model.isLoadingCapabilities,
                    error: model.mcpError,
                    searchPrompt: "settings.agents.searchMCP",
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

            Section {
                if let saveError {
                    Text(saveError)
                        .foregroundStyle(.red)
                }

                HStack(spacing: 10) {
                    Spacer()

                    if isDirty {
                        Text("settings.agents.unsaved")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }

                    if isDirty {
                        Button("common.cancel") {
                            cancelEditing()
                        }
                    }

                    Button("common.save") {
                        Task { await save() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!canSave)
                }
            }
        }
        .formStyle(.grouped)
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

    @ViewBuilder
    private var engineField: some View {
        if model.engines.isEmpty {
            TextField("settings.agents.engine", text: $role.agentType)
        } else {
            Picker("settings.agents.engine", selection: $role.agentType) {
                if role.agentType.isEmpty {
                    Text("settings.agents.engine.none").tag("")
                }
                ForEach(model.engines) { engine in
                    Text(engineLabel(engine)).tag(engine.agentType)
                }
                if !role.agentType.isEmpty, !model.engines.contains(where: { $0.agentType == role.agentType }) {
                    Text(role.agentType).tag(role.agentType)
                }
            }
        }
    }

    private var providerField: some View {
        VStack(alignment: .leading, spacing: 4) {
            Picker("settings.agents.provider", selection: providerBinding) {
                if role.modelProvider.isEmpty {
                    Text("settings.agents.model.none").tag("")
                }
                ForEach(providerChoices) { choice in
                    Text(choice.name).tag(choice.id)
                }
                if !role.modelProvider.isEmpty, !providerChoices.contains(where: { $0.id == resolvedProviderID }) {
                    Text(role.modelProvider).tag(role.modelProvider)
                }
            }
            .disabled(providerChoices.isEmpty)

            if model.isLoadingFace {
                ProgressView().controlSize(.small)
            } else if let faceError = model.faceError {
                errorLine(faceError, retry: reloadFace)
            }
        }
    }

    private var modelField: some View {
        VStack(alignment: .leading, spacing: 4) {
            Picker("settings.agents.model", selection: $role.model) {
                if role.model.isEmpty {
                    Text("settings.agents.model.none").tag("")
                }
                ForEach(modelOptions, id: \.self) { name in
                    Text(name).tag(name)
                }
                if !role.model.isEmpty, !modelOptions.contains(role.model) {
                    Text(role.model).tag(role.model)
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

    private func markdownPreview(
        title: LocalizedStringKey,
        text: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)

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
        }
    }

    @ViewBuilder
    private var selectedEngine: Engine? {
        model.engines.first { $0.agentType == role.agentType }
    }

    private func engineStatus(_ engine: Engine) -> String {
        if let version = engine.installedVersion {
            return "v\(version)"
        }
        return engine.available
            ? String(localized: "settings.engines.available")
            : String(localized: "settings.engines.missing")
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
                let options = modelOptions(for: providerID)
                if !options.contains(role.model) {
                    role.model = options.first ?? ""
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

    private func engineLabel(_ engine: Engine) -> String {
        var parts = [engine.name]
        if !engine.available {
            parts.append(String(localized: "settings.agents.engine.missing"))
        } else if !engine.enabled {
            parts.append(String(localized: "settings.agents.engine.disabled"))
        }
        return parts.joined(separator: " · ")
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
            set: { request(.select($0)) }
        )
    }

    private var isDirty: Bool {
        role != (isCreating ? draftBaseline : (savedRole ?? .empty))
    }

    private var canSave: Bool {
        isDirty
            && !isSaving
            && !role.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !role.agentType.isEmpty
    }

    private func request(_ change: PendingChange) {
        if isDirty {
            pendingChange = change
        } else {
            apply(change)
        }
    }

    private func apply(_ change: PendingChange) {
        pendingChange = nil
        switch change {
        case let .select(id):
            isCreating = false
            selection = id
        case .create:
            startCreating()
        case let .duplicate(agent):
            beginDuplicate(agent)
        }
    }

    private func startCreating() {
        isCreating = true
        selection = nil
        var draft = AgentRole.empty
        draft.agentType = model.engines.first(where: { $0.available && $0.enabled })?.agentType
            ?? model.engines.first?.agentType
            ?? ""
        role = draft
        draftBaseline = draft
        savedRole = nil
        saveError = nil
    }

    private func beginDuplicate(_ agent: AgentRole) {
        var copy = agent
        copy.id = ""
        copy.name = String(format: String(localized: "settings.agents.copyName"), agent.name)
        isCreating = true
        selection = nil
        role = copy
        draftBaseline = copy
        savedRole = nil
        saveError = nil
    }

    private func cancelEditing() {
        saveError = nil
        if isCreating {
            isCreating = false
            selection = nil
            role = .empty
            draftBaseline = .empty
            savedRole = nil
            selectFirstIfNeeded()
        } else if let savedRole {
            role = savedRole
        }
    }

    private func selectFirstIfNeeded() {
        guard selection == nil, !isCreating, let first = model.agents.first else { return }
        selection = first.id
        loadSelection()
    }

    private func loadSelection() {
        guard !isCreating, let selection, let agent = model.agents.first(where: { $0.id == selection }) else {
            return
        }
        role = agent
        savedRole = agent
        draftBaseline = agent
        saveError = nil
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

    private func save() async {
        isSaving = true
        saveError = nil

        do {
            let saved = try await model.save(role)
            isCreating = false
            role = saved
            savedRole = saved
            draftBaseline = saved
            selection = saved.id
        } catch {
            saveError = error.localizedDescription
        }

        isSaving = false
    }

    private func performDelete(_ agent: AgentRole) async {
        pendingDelete = nil
        do {
            try await model.delete(agent.id)
            if selection == agent.id {
                selection = nil
                selectFirstIfNeeded()
            }
        } catch {
            saveError = error.localizedDescription
        }
    }
}

private struct ProviderChoice: Identifiable, Hashable {
    let id: String
    let name: String
}

private struct CapabilityMultiSelect: View {
    let title: LocalizedStringKey
    let options: [CapabilityOption]
    let knownIDs: Set<String>
    @Binding var selection: [String]
    let isLoading: Bool
    let error: String?
    let searchPrompt: LocalizedStringKey
    let onRetry: () -> Void

    @State private var showPicker = false
    @State private var query = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text(title)

                Spacer()

                Text("\(selection.count)")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Button {
                    showPicker.toggle()
                } label: {
                    Label("settings.agents.select", systemImage: "chevron.down")
                }
                .controlSize(.small)
                .popover(isPresented: $showPicker, arrowEdge: .bottom) {
                    picker
                }
            }

            if selection.isEmpty {
                Text("settings.agents.noneSelected")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(selectedOptions) { option in
                    HStack(spacing: 6) {
                        Text(option.name)

                        if !knownIDs.contains(option.id) {
                            Text("settings.agents.capabilityMissingShort")
                                .font(.caption2)
                                .foregroundStyle(.orange)
                        }

                        Spacer()

                        Button {
                            selection.removeAll { $0 == option.id }
                        } label: {
                            Image(systemName: "xmark")
                                .font(.caption2)
                        }
                        .buttonStyle(.borderless)
                        .help("common.delete")
                    }
                    .font(.callout)
                }
            }
        }
    }

    private var picker: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("", text: $query, prompt: Text(searchPrompt))
                .labelsHidden()
                .textFieldStyle(.roundedBorder)

            HStack(spacing: 12) {
                Button("settings.agents.selectAll") {
                    for option in filtered where !selection.contains(option.id) {
                        selection.append(option.id)
                    }
                }
                Button("settings.agents.clearAll") {
                    selection.removeAll()
                }
                Spacer()
                Text("\(selection.count)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.link)
            .font(.caption)

            Divider()

            if isLoading, options.isEmpty {
                ProgressView().controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error {
                VStack(alignment: .leading, spacing: 6) {
                    Text(error)
                        .font(.caption)
                        .foregroundStyle(.red)
                    Button("settings.agents.retry", action: onRetry)
                        .buttonStyle(.link)
                        .font(.caption)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else if filtered.isEmpty {
                Text("settings.agents.capabilities.empty")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(filtered) { option in
                            Toggle(isOn: binding(for: option.id)) {
                                HStack(spacing: 4) {
                                    Text(option.name)
                                    if !knownIDs.contains(option.id) {
                                        Text("settings.agents.capabilityMissingShort")
                                            .font(.caption2)
                                            .foregroundStyle(.orange)
                                    }
                                }
                            }
                            .toggleStyle(.checkbox)
                        }
                    }
                }
            }
        }
        .padding(12)
        .frame(width: 360, height: 380)
    }

    private var allOptions: [CapabilityOption] {
        var rows = options
        for id in selection where !rows.contains(where: { $0.id == id }) {
            rows.append(CapabilityOption(id: id, name: id, detail: nil))
        }
        return rows
    }

    private var selectedOptions: [CapabilityOption] {
        allOptions.filter { selection.contains($0.id) }
    }

    private var filtered: [CapabilityOption] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return allOptions }
        return allOptions.filter {
            $0.name.localizedCaseInsensitiveContains(needle)
                || $0.id.localizedCaseInsensitiveContains(needle)
        }
    }

    private func binding(for id: String) -> Binding<Bool> {
        Binding(
            get: { selection.contains(id) },
            set: { isOn in
                if isOn {
                    if !selection.contains(id) {
                        selection.append(id)
                    }
                } else {
                    selection.removeAll { $0 == id }
                }
            }
        )
    }
}
