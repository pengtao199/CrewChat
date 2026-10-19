import SwiftUI

struct AgentEditor: View {
    let existing: AgentRole?
    let engines: [Engine]
    let onSave: (AgentRole) async throws -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var role = AgentRole.empty
    @State private var skills = ""
    @State private var mcp = ""
    @State private var isSaving = false
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("settings.agents.identity") {
                    TextField("settings.agents.name", text: $role.name)
                    engineField
                    TextField("settings.agents.provider", text: $role.modelProvider)
                    TextField("settings.agents.model", text: $role.model)
                }

                Section("settings.agents.persona") {
                    TextEditor(text: $role.soul)
                        .frame(minHeight: 70)
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

                if let errorMessage {
                    Text(errorMessage).foregroundStyle(.red)
                }
            }
            .formStyle(.grouped)
            .navigationTitle(existing == nil ? "settings.agents.add" : "settings.agents.edit")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("common.cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("common.save") {
                        Task { await submit() }
                    }
                    .disabled(role.name.isEmpty || role.agentType.isEmpty || isSaving)
                }
            }
        }
        .frame(width: 520, height: 620)
        .onAppear(perform: populate)
    }

    @ViewBuilder
    private var engineField: some View {
        if engines.isEmpty {
            TextField("settings.agents.engine", text: $role.agentType)
        } else {
            Picker("settings.agents.engine", selection: $role.agentType) {
                ForEach(engines) { engine in
                    Text(engine.name).tag(engine.agentType)
                }
            }
        }
    }

    private func populate() {
        if let existing {
            role = existing
        } else {
            role = .empty
            role.agentType = engines.first?.agentType ?? ""
        }
        skills = role.skills.joined(separator: ", ")
        mcp = role.mcp.joined(separator: ", ")
    }

    private func submit() async {
        isSaving = true
        errorMessage = nil

        var draft = role
        draft.skills = split(skills)
        draft.mcp = split(mcp)

        do {
            try await onSave(draft)
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }

        isSaving = false
    }

    private func split(_ raw: String) -> [String] {
        raw
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }
}
