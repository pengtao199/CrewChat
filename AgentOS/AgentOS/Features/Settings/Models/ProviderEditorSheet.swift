import SwiftUI

struct ProviderEditorSheet: View {
    let existing: ModelProvider?
    let model: ModelsSettingsViewModel
    let onFinish: (ModelProvider?) -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var presetID = "custom"
    @State private var name: String
    @State private var baseURL: String
    @State private var protocolName: String
    @State private var credentials: [CredentialRow]
    @State private var availableModels: [String]
    @State private var selectedModels: Set<String>
    @State private var availableQuery = ""
    @State private var addedQuery = ""
    @State private var manualModel = ""
    @State private var isSaving = false
    @State private var isProbing = false
    @State private var error: String?
    @State private var probeNote: String?
    @State private var importOpen = false

    private static let protocols: [(id: String, label: String)] = [
        ("openai_chat_completions", "OpenAI Chat"),
        ("openai_responses", "OpenAI Responses"),
        ("anthropic_messages", "Anthropic Messages"),
        ("gemini_generate_content", "Gemini"),
        ("gemini_interactions", "Gemini Interactions"),
    ]

    init(
        existing: ModelProvider?,
        model: ModelsSettingsViewModel,
        onFinish: @escaping (ModelProvider?) -> Void
    ) {
        self.existing = existing
        self.model = model
        self.onFinish = onFinish

        let storedType = existing?.type ?? ""
        let type = storedType.isEmpty || storedType == "—"
            ? "openai_chat_completions"
            : storedType
        let storedURL = existing?.baseURL ?? ""
        let storedModels = existing?.models ?? []

        _name = State(initialValue: existing?.name ?? "")
        _baseURL = State(initialValue: storedURL == "—" ? "" : storedURL)
        _protocolName = State(initialValue: type)
        _availableModels = State(initialValue: [])
        _selectedModels = State(initialValue: Set(storedModels))

        let rows = existing?.credentials.map { credential in
            CredentialRow(
                credentialID: credential.id,
                name: credential.name,
                apiKey: credential.apiKey,
                enabled: credential.enabled,
                priorityText: credential.priority.map(String.init) ?? "",
                weightText: credential.weight.map(Self.formatNumber) ?? "",
                limitsText: credential.limits?.prettyText ?? "",
                advanced: credential.priority != nil
                    || credential.weight != nil
                    || credential.limits != nil
            )
        } ?? []
        _credentials = State(initialValue: rows.isEmpty ? [CredentialRow()] : rows)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(existing == nil ? "settings.models.add" : "settings.models.edit")
                    .font(.title3.weight(.semibold))
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 14)

            Form {
                Section("settings.models.basic") {
                    LabeledContent("settings.models.preset") {
                        Picker("", selection: $presetID) {
                            Text("settings.models.preset.custom").tag("custom")
                            ForEach(model.presets) { preset in
                                Text(preset.name).tag(preset.id)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .help("settings.models.presetHint")
                    }

                    LabeledContent("settings.models.protocol") {
                        Picker("", selection: $protocolName) {
                            ForEach(Self.protocols, id: \.id) { option in
                                Text(option.label).tag(option.id)
                            }
                            if !Self.protocols.contains(where: { $0.id == protocolName }) {
                                Text(protocolName).tag(protocolName)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }

                    LabeledContent("settings.models.name") {
                        TextField("", text: $name)
                            .labelsHidden()
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: .infinity)
                    }

                    LabeledContent("settings.models.baseURL") {
                        TextField("", text: $baseURL)
                            .labelsHidden()
                            .textFieldStyle(.roundedBorder)
                            .textContentType(.URL)
                            .frame(maxWidth: .infinity)
                    }
                }

                Section {
                    HStack {
                        Text("settings.models.credentialsPool")
                            .font(.headline)
                        Spacer()
                        Button("settings.models.importJSON") {
                            importOpen = true
                        }
                        .controlSize(.small)

                        Button("settings.models.addKey") {
                            addCredential()
                        }
                        .controlSize(.small)
                    }

                    ForEach($credentials) { $row in
                        credentialRow($row)
                    }
                } footer: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("settings.models.credentialsHint")
                        if credentials.contains(where: { !$0.limitsIsValid }) {
                            Text("settings.models.limitsInvalid")
                                .foregroundStyle(.red)
                        }
                    }
                    .foregroundStyle(.secondary)
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
                    .frame(height: 240)
                } header: {
                    Text("settings.models.selectModels")
                } footer: {
                    Text("settings.models.selectModelsHint")
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
                Button {
                    Task { await probe() }
                } label: {
                    if isProbing {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("settings.models.testConnection", systemImage: "bolt.horizontal.circle")
                    }
                }
                .disabled(isProbing || baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !hasKey)

                Spacer()

                Button("common.cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button(existing == nil ? "settings.models.create" : "common.save") {
                    Task { await save() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canSave)
            }
            .padding(14)
        }
        .frame(width: 880, height: 720)
        .onChange(of: presetID) { _, value in
            applyPreset(value)
        }
        .sheet(isPresented: $importOpen) {
            ImportCredentialsSheet { rows in
                if credentials.count == 1,
                   credentials[0].apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                   credentials[0].name == "Key 1" {
                    credentials.removeAll()
                }
                credentials.append(contentsOf: rows)
            }
        }
    }

    private func field<Content: View>(
        _ title: LocalizedStringKey,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title)
                .font(.caption2)
                .foregroundStyle(.secondary)
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func credentialRow(_ row: Binding<CredentialRow>) -> some View {
        let value = row.wrappedValue
        let index = credentials.firstIndex(where: { $0.id == value.id }) ?? 0

        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                TextField("", text: row.name, prompt: Text("Key \(index + 1)"))
                    .labelsHidden()
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 120)

                HStack(spacing: 2) {
                    Group {
                        if value.reveal {
                            TextField(
                                "",
                                text: row.apiKey,
                                prompt: Text("settings.models.keyPlaceholder")
                            )
                        } else {
                            SecureField(
                                "",
                                text: row.apiKey,
                                prompt: Text("settings.models.keyPlaceholder")
                            )
                        }
                    }
                    .labelsHidden()
                    .textFieldStyle(.roundedBorder)

                    Button {
                        row.wrappedValue.reveal.toggle()
                    } label: {
                        Image(systemName: value.reveal ? "eye.slash" : "eye")
                    }
                    .buttonStyle(.borderless)
                }

                Button {
                    row.wrappedValue.advanced.toggle()
                } label: {
                    Image(systemName: "slider.horizontal.3")
                }
                .buttonStyle(.borderless)
                .foregroundStyle(value.advanced ? Color.accentColor : Color.secondary)
                .help("settings.models.advanced")

                Button {
                    move(value, by: -1)
                } label: {
                    Image(systemName: "arrow.up")
                }
                .buttonStyle(.borderless)
                .disabled(index == 0)

                Button {
                    move(value, by: 1)
                } label: {
                    Image(systemName: "arrow.down")
                }
                .buttonStyle(.borderless)
                .disabled(index == credentials.count - 1)

                Button {
                    remove(value)
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.red)
                .disabled(credentials.count <= 1)
            }

            if value.advanced {
                HStack(alignment: .top, spacing: 10) {
                    field("settings.models.priority") {
                        TextField("", text: row.priorityText, prompt: Text("1"))
                            .labelsHidden()
                            .textFieldStyle(.roundedBorder)
                    }
                    field("settings.models.weight") {
                        TextField("", text: row.weightText, prompt: Text("1"))
                            .labelsHidden()
                            .textFieldStyle(.roundedBorder)
                    }
                }

                field("settings.models.limits") {
                    TextEditor(text: row.limitsText)
                        .font(.system(.caption, design: .monospaced))
                        .frame(height: 56)
                        .overlay(
                            RoundedRectangle(cornerRadius: 6)
                                .stroke(.separator)
                        )
                }
            }
        }
        .padding(10)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(.separator)
        )
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
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(.separator)
        )
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
        .disabled(isProbing || baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !hasKey)
    }

    private var customAddRow: some View {
        HStack(spacing: 6) {
            TextField(
                "",
                text: $manualModel,
                prompt: Text("settings.models.modelIDPlaceholder")
            )
            .labelsHidden()
            .textFieldStyle(.roundedBorder)

            Button {
                addManualModel()
            } label: {
                Label("settings.models.customModel", systemImage: "plus")
            }
            .controlSize(.small)
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
                Image(systemName: "xmark")
                    .font(.caption)
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.red)
            .help("common.delete")
        }
        .padding(.vertical, 5)
        .padding(.horizontal, 6)
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

    private func addCredential() {
        credentials.append(CredentialRow(name: "Key \(credentials.count + 1)"))
    }

    private func move(_ row: CredentialRow, by offset: Int) {
        guard let index = credentials.firstIndex(where: { $0.id == row.id }) else { return }
        let target = index + offset
        guard credentials.indices.contains(target) else { return }
        credentials.swapAt(index, target)
    }

    private func remove(_ row: CredentialRow) {
        guard credentials.count > 1 else { return }
        credentials.removeAll { $0.id == row.id }
    }

    private var hasKey: Bool {
        credentials.contains {
            !$0.apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    private var canSave: Bool {
        !isSaving
            && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !baseURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && hasKey
            && credentials.allSatisfy(\.limitsIsValid)
    }

    private func applyPreset(_ id: String) {
        guard id != "custom",
              let preset = model.presets.first(where: { $0.id == id })
        else {
            return
        }

        name = preset.name
        if let endpoint = preset.endpoints.first {
            baseURL = endpoint.baseURL
            if let protocolName = endpoint.protocols.first {
                self.protocolName = protocolName
            }
        }
        availableModels = preset.defaultModels
        selectedModels = Set(preset.defaultModels)
        probeNote = nil
    }

    private func probe() async {
        error = nil
        probeNote = nil
        isProbing = true

        let key = credentials.compactMap { row -> String? in
            let trimmed = row.apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }.first

        guard let key else {
            error = String(localized: "settings.models.noKey")
            isProbing = false
            return
        }

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

        let drafts = credentials.enumerated().compactMap { index, row -> ModelCredentialDraft? in
            let key = row.apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
            // A blank row the user added but never filled in is not a credential.
            if key.isEmpty && row.credentialID == nil {
                return nil
            }

            let trimmedName = row.name.trimmingCharacters(in: .whitespacesAndNewlines)
            return ModelCredentialDraft(
                id: row.credentialID,
                name: trimmedName.isEmpty ? "Key \(index + 1)" : trimmedName,
                apiKey: key,
                enabled: row.enabled,
                priorityText: row.priorityText.trimmingCharacters(in: .whitespacesAndNewlines),
                weightText: row.weightText.trimmingCharacters(in: .whitespacesAndNewlines),
                limitsText: row.limitsText.trimmingCharacters(in: .whitespacesAndNewlines)
            )
        }

        let draft = ModelProviderDraft(
            originalID: existing?.id,
            name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            baseURL: baseURL.trimmingCharacters(in: .whitespacesAndNewlines),
            protocolName: protocolName,
            enabled: true,
            models: selectedModels.sorted(),
            credentials: drafts
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

    private static func formatNumber(_ value: Double) -> String {
        if value.rounded() == value {
            return String(Int(value))
        }
        return String(value)
    }
}

private struct CredentialRow: Identifiable {
    let id = UUID()
    var credentialID: String?
    var name: String
    var apiKey: String
    var enabled: Bool
    var priorityText: String
    var weightText: String
    var limitsText: String
    var reveal = false
    var advanced = false

    init(
        credentialID: String? = nil,
        name: String = "Key 1",
        apiKey: String = "",
        enabled: Bool = true,
        priorityText: String = "",
        weightText: String = "",
        limitsText: String = "",
        advanced: Bool = false
    ) {
        self.credentialID = credentialID
        self.name = name
        self.apiKey = apiKey
        self.enabled = enabled
        self.priorityText = priorityText
        self.weightText = weightText
        self.limitsText = limitsText
        self.advanced = advanced
    }

    var limitsIsValid: Bool {
        let trimmed = limitsText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return true }
        guard let value = try? JSONValue.parse(trimmed) else { return false }
        return value.objectValue != nil
    }
}

private struct ImportCredentialsSheet: View {
    let onImport: ([CredentialRow]) -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var text = ""
    @State private var error: String?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("settings.models.importTitle")
                    .font(.title3.weight(.semibold))
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 14)

            Form {
                Section {
                    Text("settings.models.importHint")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)

                    TextEditor(text: $text)
                        .font(.system(.caption, design: .monospaced))
                        .frame(minHeight: 220)
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
                Spacer()
                Button("common.cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button("settings.models.import") {
                    performImport()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
            .padding(14)
        }
        .frame(minWidth: 620, minHeight: 520)
    }

    private func performImport() {
        error = nil

        do {
            let rows = try parse(text)
            onImport(rows)
            dismiss()
        } catch let error as ImportError {
            self.error = error.errorDescription
        } catch {
            self.error = String(localized: "settings.models.importFailed")
        }
    }

    private func parse(_ text: String) throws -> [CredentialRow] {
        let root = try JSONValue.parse(text)

        let items: [JSONValue]
        switch root {
        case let .array(values):
            items = values
        case let .object(object):
            if case let .array(values)? = object["credentials"] {
                items = values
            } else {
                items = [root]
            }
        default:
            items = []
        }

        let rows = items.enumerated().compactMap { index, value -> CredentialRow? in
            guard let object = value.objectValue else { return nil }

            let key = ["api_key", "apiKey", "apikey", "key"]
                .lazy
                .compactMap { object[$0]?.stringValue }
                .first ?? ""
            guard !key.isEmpty else { return nil }

            let name = ["name", "label"]
                .lazy
                .compactMap { object[$0]?.stringValue }
                .first ?? "Key \(index + 1)"

            let priority = object["priority"]?.numberValue.map(Self.formatNumber) ?? ""
            let weight = object["weight"]?.numberValue.map(Self.formatNumber) ?? ""
            let limits = object["limits"].flatMap { value -> String? in
                guard value.objectValue != nil else { return nil }
                return value.prettyText
            } ?? ""

            return CredentialRow(
                name: name,
                apiKey: key,
                priorityText: priority,
                weightText: weight,
                limitsText: limits,
                advanced: !priority.isEmpty || !weight.isEmpty || !limits.isEmpty
            )
        }

        guard !rows.isEmpty else {
            throw ImportError.noKeys
        }
        return rows
    }

    private static func formatNumber(_ value: Double) -> String {
        if value.rounded() == value {
            return String(Int(value))
        }
        return String(value)
    }
}

private enum ImportError: LocalizedError {
    case noKeys

    var errorDescription: String? {
        String(localized: "settings.models.importNoKeys")
    }
}
