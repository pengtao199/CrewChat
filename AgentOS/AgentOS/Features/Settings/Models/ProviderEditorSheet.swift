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
    @State private var availableQuery = ""
    @State private var addedQuery = ""
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
        _availableModels = State(initialValue: [])
        _selectedModels = State(initialValue: Set(storedModels))
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(existing == nil ? "settings.models.new" : "settings.models.edit")
                    .font(.title3.weight(.semibold))
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 14)

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
                    if let probeNote {
                        Text(probeNote)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    HStack(alignment: .top, spacing: 10) {
                        modelPane(
                            title: "settings.models.availableTitle",
                            subtitle: "settings.models.availableSubtitle",
                            count: availableModels.count,
                            query: $availableQuery,
                            searchPrompt: "settings.models.searchAvailable",
                            emptyText: availableModels.isEmpty
                                ? "settings.models.availableEmpty"
                                : "settings.models.noMatch",
                            isEmpty: filteredAvailable.isEmpty,
                            headerAction: AnyView(fetchButton)
                        ) {
                            ForEach(filteredAvailable, id: \.self) { item in
                                availableRow(item)
                            }
                        }

                        modelPane(
                            title: "settings.models.addedTitle",
                            subtitle: "settings.models.addedSubtitle",
                            count: selectedModels.count,
                            query: $addedQuery,
                            searchPrompt: "settings.models.searchAdded",
                            emptyText: selectedModels.isEmpty
                                ? "settings.models.addedEmpty"
                                : "settings.models.noMatch",
                            isEmpty: filteredSelected.isEmpty,
                            topContent: AnyView(customAddRow)
                        ) {
                            ForEach(filteredSelected, id: \.self) { item in
                                selectedRow(item)
                            }
                        }
                    }
                    .frame(height: 330)
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
        .frame(minWidth: 820, minHeight: 700)
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

    private var filteredAvailable: [String] {
        filter(availableModels, query: availableQuery)
    }

    private var filteredSelected: [String] {
        filter(selectedModels.sorted(), query: addedQuery)
    }

    private func filter(_ models: [String], query: String) -> [String] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return models }
        return models.filter { $0.localizedCaseInsensitiveContains(needle) }
    }

    private var canAddManualModel: Bool {
        let value = manualModel.trimmingCharacters(in: .whitespacesAndNewlines)
        return !value.isEmpty && !selectedModels.contains(value)
    }

    private func isAdded(_ model: String) -> Bool {
        selectedModels.contains(model)
    }

    private func add(_ model: String) {
        selectedModels.insert(model)
    }

    private func remove(_ model: String) {
        selectedModels.remove(model)
    }

    private func addManualModel() {
        let value = manualModel.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        selectedModels.insert(value)
        manualModel = ""
    }

    private var fetchButton: some View {
        Button {
            Task { await probe() }
        } label: {
            if isProbing {
                ProgressView().controlSize(.mini)
            } else {
                Label("settings.models.fetch", systemImage: "arrow.down.circle")
            }
        }
        .buttonStyle(.borderless)
        .font(.caption)
        .disabled(isProbing || baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
    }

    private var customAddRow: some View {
        HStack(spacing: 6) {
            TextField(
                "",
                text: $manualModel,
                prompt: Text("settings.models.manualModel")
            )
            .labelsHidden()
            .textFieldStyle(.roundedBorder)

            Button {
                addManualModel()
            } label: {
                Image(systemName: "plus.circle")
            }
            .buttonStyle(.borderless)
            .disabled(!canAddManualModel)
        }
    }

    @ViewBuilder
    private func availableRow(_ item: String) -> some View {
        Button {
            add(item)
        } label: {
            HStack(spacing: 6) {
                Text(item)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 6)
                if isAdded(item) {
                    Label("settings.models.added", systemImage: "checkmark")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                } else {
                    Image(systemName: "plus.circle")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .contentShape(Rectangle())
            .padding(.vertical, 5)
            .padding(.horizontal, 6)
        }
        .buttonStyle(.plain)
        .disabled(isAdded(item))
        .opacity(isAdded(item) ? 0.55 : 1)
    }

    @ViewBuilder
    private func selectedRow(_ item: String) -> some View {
        HStack(spacing: 6) {
            Text(item)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 6)
            Button {
                remove(item)
            } label: {
                Image(systemName: "minus.circle")
            }
            .buttonStyle(.borderless)
            .help("common.delete")
        }
        .padding(.vertical, 5)
        .padding(.horizontal, 6)
    }

    private func modelPane<Rows: View>(
        title: LocalizedStringKey,
        subtitle: LocalizedStringKey,
        count: Int,
        query: Binding<String>,
        searchPrompt: LocalizedStringKey,
        emptyText: LocalizedStringKey,
        isEmpty: Bool,
        headerAction: AnyView? = nil,
        topContent: AnyView? = nil,
        @ViewBuilder rows: () -> Rows
    ) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.headline)
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if let headerAction {
                    headerAction
                }
                Text("\(count)")
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
            .padding(10)

            if let topContent {
                topContent
                    .padding(.horizontal, 10)
                    .padding(.bottom, 8)
            }

            TextField("", text: query, prompt: Text(searchPrompt))
                .labelsHidden()
                .textFieldStyle(.roundedBorder)
                .padding(.horizontal, 10)
                .padding(.bottom, 8)

            Divider()

            if isEmpty {
                Text(emptyText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                Spacer(minLength: 0)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 1) {
                        rows()
                    }
                    .padding(4)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.background, in: RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(.separator)
        )
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
            availableModels = Array(Set(result.models)).sorted()

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
