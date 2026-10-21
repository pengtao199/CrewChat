import SwiftUI

struct ProviderEditorSheet: View {
    let existing: CCRProvider?
    let model: ModelsSettingsViewModel
    let onFinish: (CCRProvider?) -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var name: String
    @State private var baseURL: String
    @State private var protocolName: String
    @State private var apiKey = ""
    @State private var clearKey = false
    @State private var enabled: Bool
    @State private var availableModels: [String]
    @State private var selectedModels: Set<String>
    @State private var manualModel = ""
    @State private var modelQuery = ""
    @State private var isSaving = false
    @State private var isProbing = false
    @State private var error: String?
    @State private var probeNote: String?
    @State private var confirmDelete = false

    private static let protocols: [(id: String, label: String)] = [
        ("openai_chat_completions", "OpenAI Chat"),
        ("openai_responses", "OpenAI Responses"),
        ("anthropic_messages", "Anthropic Messages"),
        ("gemini_generate_content", "Gemini"),
        ("gemini_interactions", "Gemini Interactions"),
    ]

    init(
        existing: CCRProvider?,
        model: ModelsSettingsViewModel,
        onFinish: @escaping (CCRProvider?) -> Void
    ) {
        self.existing = existing
        self.model = model
        self.onFinish = onFinish

        let storedType = existing?.type ?? ""
        let type = Self.protocols.contains(where: { $0.id == storedType })
            ? storedType
            : "openai_chat_completions"
        let storedModels = existing?.models ?? []

        _name = State(initialValue: existing?.name ?? "")
        _baseURL = State(initialValue: existing?.baseURL ?? "")
        _protocolName = State(initialValue: type)
        _enabled = State(initialValue: existing?.enabled ?? true)
        _availableModels = State(initialValue: storedModels)
        _selectedModels = State(initialValue: Set(storedModels))
    }

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section("settings.models.basic") {
                    TextField("settings.models.name", text: $name)
                        .disabled(existing != nil)

                    if existing != nil {
                        Text("settings.models.nameLocked")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    TextField("settings.models.baseURL", text: $baseURL)
                        .textContentType(.URL)

                    Picker("settings.models.protocol", selection: $protocolName) {
                        ForEach(Self.protocols, id: \.id) { option in
                            Text(option.label).tag(option.id)
                        }
                    }

                    SecureField(
                        existing == nil
                            ? "settings.models.apiKey"
                            : "settings.models.apiKeyKeep",
                        text: $apiKey
                    )
                    .disabled(clearKey)

                    if existing != nil, !hasIndependentCredentials {
                        Toggle("settings.models.clearKey", isOn: $clearKey)
                    }

                    Toggle("settings.models.enabledToggle", isOn: $enabled)
                }

                Section {
                    HStack {
                        Button {
                            Task { await probe() }
                        } label: {
                            if isProbing {
                                ProgressView().controlSize(.small)
                            } else {
                                Label("settings.models.probe", systemImage: "bolt.horizontal.circle")
                            }
                        }
                        .disabled(isProbing || baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                        Spacer()

                        Text(String(format: String(localized: "settings.models.selectedCount"), selectedModels.count))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    if let probeNote {
                        Text(probeNote)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    if availableModels.isEmpty {
                        Text("settings.models.modelsEmpty")
                            .foregroundStyle(.secondary)
                    } else {
                        if availableModels.count > 12 {
                            TextField("settings.models.searchModels", text: $modelQuery)
                        }

                        ScrollView {
                            LazyVStack(alignment: .leading, spacing: 4) {
                                ForEach(filteredModels, id: \.self) { item in
                                    Toggle(item, isOn: binding(for: item))
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .frame(minHeight: 100, maxHeight: 220)
                    }

                    HStack(spacing: 8) {
                        TextField("settings.models.manualModel", text: $manualModel)
                        Button {
                            addManualModel()
                        } label: {
                            Image(systemName: "plus.circle")
                        }
                        .buttonStyle(.borderless)
                        .disabled(manualModel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                } header: {
                    Text("settings.models.models")
                } footer: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("settings.models.secretHint")
                        if hasIndependentCredentials {
                            Text("settings.models.keyScope")
                        }
                    }
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
                if let existing {
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
        .frame(minWidth: 560, minHeight: 620)
        .navigationTitle(existing == nil ? "settings.models.new" : "settings.models.edit")
        .alert("settings.models.delete.title", isPresented: $confirmDelete) {
            Button("common.delete", role: .destructive) {
                Task { await remove() }
            }
            Button("common.cancel", role: .cancel) {}
        } message: {
            Text(String(format: String(localized: "settings.models.delete.message"), existing?.name ?? ""))
        }
    }

    private var hasIndependentCredentials: Bool {
        existing?.credentials.contains(where: { $0.id != "api_key" }) == true
    }

    private var canSave: Bool {
        !isSaving
            && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !selectedModels.isEmpty
    }

    private var filteredModels: [String] {
        let needle = modelQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return availableModels }
        return availableModels.filter { $0.localizedCaseInsensitiveContains(needle) }
    }

    private func binding(for item: String) -> Binding<Bool> {
        Binding(
            get: { selectedModels.contains(item) },
            set: { isOn in
                if isOn {
                    selectedModels.insert(item)
                } else {
                    selectedModels.remove(item)
                }
            }
        )
    }

    private func addManualModel() {
        let value = manualModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        if !availableModels.contains(value) {
            availableModels.append(value)
            availableModels.sort()
        }
        selectedModels.insert(value)
        manualModel = ""
    }

    private func probe() async {
        error = nil
        probeNote = nil
        isProbing = true

        let typedKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        let storedKey = existing?.credentials.first?.apiKey
        let key = typedKey.isEmpty ? storedKey : typedKey

        do {
            let result = try await model.probeProvider(baseURL: baseURL, apiKey: key)
            var merged = Set(availableModels)
            merged.formUnion(result.models)
            availableModels = merged.sorted()

            if existing == nil {
                selectedModels.formUnion(result.models)
            }

            var note = String(
                format: String(localized: "settings.models.probeOK"),
                result.models.count
            )
            if let protocolName = result.protocolName, !protocolName.isEmpty {
                note += " · \(protocolName)"
            }
            probeNote = note
        } catch {
            self.error = error.localizedDescription
        }

        isProbing = false
    }

    private func save() async {
        error = nil
        isSaving = true

        let draft = CCRProviderDraft(
            originalID: existing?.id,
            name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            baseURL: baseURL.trimmingCharacters(in: .whitespacesAndNewlines),
            protocolName: protocolName,
            apiKey: apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? nil
                : apiKey,
            clearAPIKey: clearKey,
            enabled: enabled,
            models: selectedModels.sorted()
        )

        do {
            let saved = try await model.saveProvider(draft)
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
            try await model.deleteProvider(id: existing.id)
            onFinish(nil)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }

        isSaving = false
    }
}
