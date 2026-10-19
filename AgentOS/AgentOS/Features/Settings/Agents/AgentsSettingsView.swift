import SwiftUI

struct AgentsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var isCreating = false
    @State private var role = AgentRole.empty
    @State private var isSaving = false
    @State private var saveError: String?
    @State private var previewSoul = false
    @State private var previewRules = false
    @State private var skillsExpanded = false
    @State private var mcpExpanded = false

    private var model: AgentsSettingsViewModel { environment.roster }

    var body: some View {
        MasterDetailLayout {
            List(model.agents, selection: $selection) { agent in
                VStack(alignment: .leading, spacing: 2) {
                    Text(agent.name)
                    Text(agent.agentType)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .tag(agent.id)
                .contextMenu {
                    Button("common.delete", role: .destructive) {
                        Task { await delete(agent.id) }
                    }
                }
            }
        } right: {
            detail
        }
        .navigationTitle("settings.agents")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    startCreating()
                } label: {
                    Label("common.add", systemImage: "plus")
                }
            }
        }
        .onChange(of: selection) { _, _ in
            loadSelection()
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
            await reloadForDraft()
        }
        .onChange(of: role.agentType) { _, _ in
            Task { await reloadForDraft() }
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let message = model.errorMessage {
            ContentUnavailableView(
                "common.error",
                systemImage: "exclamationmark.triangle",
                description: Text(message)
            )
        } else if isCreating || selection != nil {
            editor
        } else if model.agents.isEmpty {
            ContentUnavailableView(
                "settings.agents.empty.title",
                systemImage: "person.2",
                description: Text("settings.agents.empty.description")
            )
        } else {
            SelectItemPlaceholder()
        }
    }

    private var editor: some View {
        Form {
            Section("settings.agents.identity") {
                TextField("settings.agents.name", text: $role.name)
                engineField
                providerField
                modelField
            }

            Section("settings.agents.persona") {
                markdownField(
                    title: "settings.agents.soul",
                    text: $role.soul,
                    preview: $previewSoul
                )
                markdownField(
                    title: "settings.agents.rules",
                    text: $role.rules,
                    preview: $previewRules
                )
            }

            Section("settings.agents.capabilities") {
                DisclosureGroup(isExpanded: $skillsExpanded) {
                    ForEach(skillOptions) { option in
                        Toggle(option.name, isOn: binding(for: option.id, in: $role.skills))
                    }
                } label: {
                    LabeledContent("settings.agents.skills", value: "\(role.skills.count)")
                }

                DisclosureGroup(isExpanded: $mcpExpanded) {
                    ForEach(mcpOptions) { option in
                        Toggle(option.name, isOn: binding(for: option.id, in: $role.mcp))
                    }
                } label: {
                    LabeledContent("settings.agents.mcp", value: "\(role.mcp.count)")
                }
            }

            Section("settings.agents.permissions") {
                TextField("settings.agents.permissionMode", text: $role.permissionMode)
                Toggle("settings.agents.autoApprove", isOn: $role.autoApprove)
            }

            Section {
                if let saveError {
                    Text(saveError).foregroundStyle(.red)
                }
                HStack {
                    if !isCreating, let selection {
                        Button("common.delete", role: .destructive) {
                            Task { await delete(selection) }
                        }
                    }
                    Spacer()
                    Button("common.save") {
                        Task { await save() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(role.name.isEmpty || role.agentType.isEmpty || isSaving)
                }
            }
        }
        .formStyle(.grouped)
    }

    @ViewBuilder
    private var engineField: some View {
        if model.engines.isEmpty {
            TextField("settings.agents.engine", text: $role.agentType)
        } else {
            Picker("settings.agents.engine", selection: $role.agentType) {
                ForEach(model.engines) { engine in
                    Text(engine.name).tag(engine.agentType)
                }
            }
        }
    }

    private var providerField: some View {
        VStack(alignment: .leading, spacing: 4) {
            Picker("settings.agents.provider", selection: $role.modelProvider) {
                ForEach(providerOptions, id: \.self) { provider in
                    Text(provider).tag(provider)
                }
            }
            .disabled(providerOptions.isEmpty)

            if let hint = faceHint {
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var modelField: some View {
        Picker("settings.agents.model", selection: $role.model) {
            ForEach(modelOptions, id: \.self) { name in
                Text(name).tag(name)
            }
        }
        .disabled(modelOptions.isEmpty)
        .onChange(of: role.modelProvider) { _, _ in
            let options = modelOptions
            guard !options.isEmpty else { return }
            if !options.contains(role.model) {
                role.model = options.first ?? ""
            }
        }
    }

    private var providerOptions: [String] {
        let face = model.face
        var names = face?.models.map(\.provider) ?? []
        names = names.reduce(into: []) { result, name in
            if !result.contains(name) { result.append(name) }
        }
        if !role.modelProvider.isEmpty, !names.contains(role.modelProvider) {
            names.insert(role.modelProvider, at: 0)
        }
        return names
    }

    private var modelOptions: [String] {
        let face = model.face
        var names = (face?.models ?? [])
            .filter { $0.provider == role.modelProvider }
            .map(\.model)
        if !role.model.isEmpty, !names.contains(role.model) {
            names.insert(role.model, at: 0)
        }
        return names
    }

    private var faceHint: String? {
        guard let face = model.face else { return nil }
        if !face.supported {
            return face.reason ?? String(localized: "settings.agents.modelUnsupported")
        }
        if face.models.isEmpty {
            return face.reason
        }
        return nil
    }

    private func markdownField(
        title: LocalizedStringKey,
        text: Binding<String>,
        preview: Binding<Bool>
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(title)
                Spacer()
                Picker("", selection: preview) {
                    Text("settings.agents.edit").tag(false)
                    Text("settings.agents.preview").tag(true)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(width: 140)
            }

            if preview.wrappedValue {
                ScrollView {
                    Text(markdown(text.wrappedValue))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 80, maxHeight: 160)
            } else {
                TextEditor(text: text)
                    .frame(minHeight: 80)
            }
        }
    }

    private func markdown(_ text: String) -> AttributedString {
        var result = AttributedString()
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        for (index, line) in lines.enumerated() {
            if index > 0 {
                result += AttributedString("\n")
            }
            let parsed = (try? AttributedString(
                markdown: String(line),
                options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
            )) ?? AttributedString(String(line))
            result += parsed
        }
        return result
    }

    private var skillOptions: [CapabilityOption] {
        union(model.skills, role.skills)
    }

    private var mcpOptions: [CapabilityOption] {
        union(model.mcpServers, role.mcp)
    }

    private func union(_ options: [CapabilityOption], _ selected: [String]) -> [CapabilityOption] {
        var all = options
        for id in selected where !all.contains(where: { $0.id == id }) {
            all.append(CapabilityOption(id: id, name: id, detail: nil))
        }
        return all
    }

    private func binding(for id: String, in values: Binding<[String]>) -> Binding<Bool> {
        Binding(
            get: { values.wrappedValue.contains(id) },
            set: { isOn in
                if isOn {
                    if !values.wrappedValue.contains(id) {
                        values.wrappedValue.append(id)
                    }
                } else {
                    values.wrappedValue.removeAll { $0 == id }
                }
            }
        )
    }

    private func reloadForDraft() async {
        await model.loadFace(agentType: role.agentType)
        await model.loadCapabilities(agentType: role.agentType)
    }

    private func startCreating() {
        selection = nil
        isCreating = true
        role = .empty
        role.agentType = model.engines.first?.agentType ?? ""
        saveError = nil
    }

    private func selectFirstIfNeeded() {
        if selection == nil, !model.agents.isEmpty {
            selection = model.agents.first?.id
            loadSelection()
        }
    }

    private func loadSelection() {
        guard !isCreating, let selection, let agent = model.agents.first(where: { $0.id == selection }) else { return }
        role = agent
        saveError = nil
        Task { await reloadForDraft() }
    }

    private func save() async {
        isSaving = true
        saveError = nil

        do {
            let saved = try await model.save(role)
            isCreating = false
            selection = saved.id
            loadSelection()
        } catch {
            saveError = error.localizedDescription
        }

        isSaving = false
    }

    private func delete(_ id: String) async {
        await model.delete(id)
        if selection == id {
            selection = nil
            selectFirstIfNeeded()
        }
    }
}
