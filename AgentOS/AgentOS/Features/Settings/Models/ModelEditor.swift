import SwiftUI

struct ModelEditor: View {
    let existing: ModelProvider?
    let engines: [Engine]
    let onSave: (ModelProviderDraft) async throws -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var apiURL = ""
    @State private var apiKey = ""
    @State private var agentType = ""
    @State private var model = ""
    @State private var isSaving = false
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            Form {
                TextField("settings.models.name", text: $name)
                TextField("settings.models.url", text: $apiURL)
                SecureField(existing == nil ? "settings.models.key" : "settings.models.key.keep", text: $apiKey)
                engineField

                Section {
                    TextField("settings.models.model", text: $model)
                } footer: {
                    Text("settings.models.model.hint")
                        .foregroundStyle(.secondary)
                }

                if let errorMessage {
                    Text(errorMessage).foregroundStyle(.red)
                }
            }
            .formStyle(.grouped)
            .navigationTitle(existing == nil ? "settings.models.add" : "settings.models.edit")
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("common.cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("common.save") {
                        Task { await submit() }
                    }
                    .disabled(!canSave || isSaving)
                }
            }
        }
        .frame(width: 460, height: 360)
        .onAppear(perform: populate)
    }

    @ViewBuilder
    private var engineField: some View {
        if engines.isEmpty {
            TextField("settings.models.engine", text: $agentType)
        } else {
            Picker("settings.models.engine", selection: $agentType) {
                ForEach(engines) { engine in
                    Text(engine.name).tag(engine.agentType)
                }
            }
        }
    }

    private var canSave: Bool {
        !name.isEmpty && !apiURL.isEmpty && !agentType.isEmpty && (existing != nil || !apiKey.isEmpty)
    }

    private func populate() {
        guard let existing else {
            agentType = engines.first?.agentType ?? ""
            return
        }
        name = existing.name
        apiURL = existing.apiURL
        agentType = existing.agentType
        model = existing.model ?? ""
    }

    private func submit() async {
        isSaving = true
        errorMessage = nil

        do {
            try await onSave(
                ModelProviderDraft(
                    name: name,
                    apiURL: apiURL,
                    apiKey: apiKey,
                    agentType: agentType,
                    model: model
                )
            )
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }

        isSaving = false
    }
}
