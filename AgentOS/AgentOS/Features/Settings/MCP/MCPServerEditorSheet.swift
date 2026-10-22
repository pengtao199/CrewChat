import SwiftUI

struct MCPServerEditorSheet: View {
    let existing: MCPServer?
    let model: MCPSettingsViewModel
    let onFinish: (MCPServer?) -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var serverID: String
    @State private var specText: String
    @State private var selectedApps: Set<String>
    @State private var error: String?
    @State private var isSaving = false
    @State private var confirmDelete = false

    private static let stdioTemplate = """
    {
      "type": "stdio",
      "command": "",
      "args": []
    }
    """

    private static let httpTemplate = """
    {
      "type": "http",
      "url": ""
    }
    """

    init(
        existing: MCPServer?,
        model: MCPSettingsViewModel,
        onFinish: @escaping (MCPServer?) -> Void
    ) {
        self.existing = existing
        self.model = model
        self.onFinish = onFinish

        _serverID = State(initialValue: existing?.id ?? "")
        _specText = State(initialValue: existing?.spec.prettyText ?? Self.stdioTemplate)
        _selectedApps = State(initialValue: Set(existing?.apps ?? []))
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(existing == nil ? "settings.mcp.new" : "settings.mcp.edit")
                    .font(.title3.weight(.semibold))
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 14)

            Form {
                Section("settings.mcp.basic") {
                    TextField("settings.mcp.id", text: $serverID)
                        .font(.system(.body, design: .monospaced))
                        .disabled(existing != nil)

                    Menu("settings.mcp.template") {
                        Button("settings.mcp.template.stdio") {
                            specText = Self.stdioTemplate
                        }
                        Button("settings.mcp.template.http") {
                            specText = Self.httpTemplate
                        }
                    }
                }

                Section {
                    TextEditor(text: $specText)
                        .font(.system(.body, design: .monospaced))
                        .frame(minHeight: 220)
                } header: {
                    Text("settings.mcp.spec")
                } footer: {
                    Text("settings.mcp.specHint")
                        .foregroundStyle(.secondary)
                }

                Section {
                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 150), spacing: 8)],
                        alignment: .leading,
                        spacing: 4
                    ) {
                        ForEach(MCPAppCatalog.all, id: \.self) { app in
                            Toggle(isOn: binding(for: app)) {
                                Text(verbatim: appLabel(app))
                            }
                            .toggleStyle(.checkbox)
                        }
                    }

                    HStack {
                        Text(String(format: String(localized: "settings.mcp.selectedApps"), selectedApps.count))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                    }
                } header: {
                    Text("settings.mcp.apps")
                } footer: {
                    Text("settings.mcp.appsHint")
                        .foregroundStyle(.secondary)
                }

                if let error {
                    Section {
                        Text(error)
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)

            Divider()

            HStack {
                if existing != nil {
                    Button("common.delete", role: .destructive) {
                        confirmDelete = true
                    }
                    .disabled(isSaving)
                }

                Spacer()

                Button("common.cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button("common.save") {
                    Task { await save() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canSave)
            }
            .padding(14)
        }
        .frame(minWidth: 760, minHeight: 780)
        .alert(
            "settings.mcp.delete.title",
            isPresented: $confirmDelete
        ) {
            Button("common.delete", role: .destructive) {
                Task { await remove() }
            }
            Button("common.cancel", role: .cancel) {}
        } message: {
            Text(String(format: String(localized: "settings.mcp.delete.message"), existing?.id ?? ""))
        }
    }

    private var canSave: Bool {
        !isSaving
            && !serverID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !selectedApps.isEmpty
    }

    private func binding(for app: String) -> Binding<Bool> {
        Binding(
            get: { selectedApps.contains(app) },
            set: { isOn in
                if isOn {
                    selectedApps.insert(app)
                } else {
                    selectedApps.remove(app)
                }
            }
        )
    }

    private func appLabel(_ app: String) -> String {
        NSLocalizedString(MCPAppCatalog.labelKey(for: app), comment: "")
    }

    private func save() async {
        let id = serverID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !id.isEmpty, !selectedApps.isEmpty else { return }

        error = nil
        isSaving = true

        let spec: JSONValue
        do {
            spec = try JSONValue.parse(specText)
        } catch {
            self.error = String(localized: "settings.mcp.jsonInvalid")
            isSaving = false
            return
        }

        guard spec.objectValue != nil else {
            self.error = String(localized: "settings.mcp.objectRequired")
            isSaving = false
            return
        }

        do {
            let saved = try await model.save(
                serverID: id,
                spec: spec,
                apps: selectedApps.sorted()
            )
            onFinish(saved)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }

        isSaving = false
    }

    private func remove() async {
        guard let existing else { return }
        error = nil
        isSaving = true

        do {
            try await model.remove(serverID: existing.id)
            onFinish(nil)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }

        isSaving = false
    }
}
