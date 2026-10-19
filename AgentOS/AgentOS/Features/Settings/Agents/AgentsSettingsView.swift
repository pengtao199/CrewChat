import SwiftUI

struct AgentsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var isCreating = false
    @State private var role = AgentRole.empty
    @State private var skills = ""
    @State private var mcp = ""
    @State private var isSaving = false
    @State private var saveError: String?

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
                TextField("settings.agents.provider", text: $role.modelProvider)
                TextField("settings.agents.model", text: $role.model)
            }

            Section("settings.agents.persona") {
                TextEditor(text: $role.soul)
                    .frame(minHeight: 80)
                TextField("settings.agents.rules", text: $role.rules)
            }

            Section("settings.agents.capabilities") {
                TextField("settings.agents.skills", text: $skills)
                TextField("settings.agents.mcp", text: $mcp)
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

    private func startCreating() {
        selection = nil
        isCreating = true
        role = .empty
        role.agentType = model.engines.first?.agentType ?? ""
        skills = ""
        mcp = ""
        saveError = nil
    }

    private func selectFirstIfNeeded() {
        if selection == nil, !model.agents.isEmpty {
            selection = model.agents.first?.id
        }
    }

    private func loadSelection() {
        guard !isCreating, let selection, let agent = model.agents.first(where: { $0.id == selection }) else { return }
        role = agent
        skills = agent.skills.joined(separator: ", ")
        mcp = agent.mcp.joined(separator: ", ")
        saveError = nil
    }

    private func save() async {
        isSaving = true
        saveError = nil

        var draft = role
        draft.skills = split(skills)
        draft.mcp = split(mcp)

        do {
            let saved = try await model.save(draft)
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

    private func split(_ raw: String) -> [String] {
        raw
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }
}
