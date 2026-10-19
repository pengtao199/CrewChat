import SwiftUI

struct ModelsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var editing: ModelProvider?
    @State private var isCreating = false

    private var model: ModelsSettingsViewModel { environment.models }

    var body: some View {
        List {
            if let message = model.errorMessage {
                Text(message).foregroundStyle(.red)
            }

            ForEach(model.providers) { provider in
                LabeledContent {
                    HStack(spacing: 8) {
                        Text(provider.agentType)
                        if let name = provider.model, !name.isEmpty {
                            Text(name)
                        }
                    }
                    .foregroundStyle(.secondary)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(provider.name)
                        Text(provider.apiKeyMasked)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .contextMenu {
                    Button("common.edit") { editing = provider }
                    Button("common.delete", role: .destructive) {
                        Task { await model.delete(provider.id) }
                    }
                }
            }
        }
        .overlay {
            if model.providers.isEmpty {
                ContentUnavailableView(
                    "settings.models.empty.title",
                    systemImage: "cube.box",
                    description: Text("settings.models.empty.description")
                )
            }
        }
        .navigationTitle("settings.models")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    isCreating = true
                } label: {
                    Label("common.add", systemImage: "plus")
                }
            }
        }
        .sheet(isPresented: $isCreating) {
            ModelEditor(existing: nil, engines: model.engines) { draft in
                try await model.create(draft)
            }
        }
        .sheet(item: $editing) { provider in
            ModelEditor(existing: provider, engines: model.engines) { draft in
                try await model.update(id: provider.id, draft)
            }
        }
        .task { await model.load() }
    }
}
