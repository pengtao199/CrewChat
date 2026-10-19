import SwiftUI

struct ModelsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: Int?
    @State private var isCreating = false
    @State private var name = ""
    @State private var apiURL = ""
    @State private var apiKey = ""
    @State private var agentType = ""
    @State private var modelName = ""
    @State private var isSaving = false
    @State private var saveError: String?

    private var model: ModelsSettingsViewModel { environment.models }

    var body: some View {
        MasterDetailLayout {
            List(model.providers, selection: $selection) { provider in
                VStack(alignment: .leading, spacing: 2) {
                    Text(provider.name)
                    Text(provider.apiKeyMasked)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .tag(provider.id)
                .contextMenu {
                    Button("common.delete", role: .destructive) {
                        Task { await delete(provider.id) }
                    }
                }
            }
        } right: {
            detail
        }
        .navigationTitle("settings.models")
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
        } else if model.providers.isEmpty {
            ContentUnavailableView(
                "settings.models.empty.title",
                systemImage: "cube.box",
                description: Text("settings.models.empty.description")
            )
        } else {
            SelectItemPlaceholder()
        }
    }

    private var editor: some View {
        Form {
            Section("settings.models.basic") {
                TextField("settings.models.name", text: $name)
                TextField("settings.models.url", text: $apiURL)
                SecureField(
                    isCreating ? "settings.models.key" : "settings.models.key.keep",
                    text: $apiKey
                )
                engineField
            }

            Section {
                TextField("settings.models.model", text: $modelName)
            } footer: {
                Text("settings.models.model.hint")
                    .foregroundStyle(.secondary)
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
                    .disabled(!canSave || isSaving)
                }
            }
        }
        .formStyle(.grouped)
    }

    @ViewBuilder
    private var engineField: some View {
        if model.engines.isEmpty {
            TextField("settings.models.engine", text: $agentType)
        } else {
            Picker("settings.models.engine", selection: $agentType) {
                ForEach(model.engines) { engine in
                    Text(engine.name).tag(engine.agentType)
                }
            }
        }
    }

    private var canSave: Bool {
        !name.isEmpty
            && !apiURL.isEmpty
            && !agentType.isEmpty
            && (isCreating ? !apiKey.isEmpty : true)
    }

    private func startCreating() {
        selection = nil
        isCreating = true
        name = ""
        apiURL = ""
        apiKey = ""
        agentType = model.engines.first?.agentType ?? ""
        modelName = ""
        saveError = nil
    }

    private func selectFirstIfNeeded() {
        if selection == nil, !model.providers.isEmpty {
            selection = model.providers.first?.id
        }
    }

    private func loadSelection() {
        guard !isCreating, let selection, let provider = model.providers.first(where: { $0.id == selection }) else { return }
        name = provider.name
        apiURL = provider.apiURL
        apiKey = ""
        agentType = provider.agentType
        modelName = provider.model ?? ""
        saveError = nil
    }

    private func save() async {
        isSaving = true
        saveError = nil

        let draft = ModelProviderDraft(
            name: name,
            apiURL: apiURL,
            apiKey: apiKey,
            agentType: agentType,
            model: modelName
        )

        do {
            if isCreating {
                let created = try await model.create(draft)
                isCreating = false
                selection = created.id
                loadSelection()
            } else if let selection {
                try await model.update(id: selection, draft)
                loadSelection()
            }
        } catch {
            saveError = error.localizedDescription
        }

        isSaving = false
    }

    private func delete(_ id: Int) async {
        await model.delete(id)
        if selection == id {
            selection = nil
            selectFirstIfNeeded()
        }
    }
}
