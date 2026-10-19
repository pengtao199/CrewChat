import SwiftUI

struct AgentsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var editing: AgentRole?
    @State private var isCreating = false

    private var model: AgentsSettingsViewModel { environment.roster }

    var body: some View {
        List {
            if let message = model.errorMessage {
                Text(message).foregroundStyle(.red)
            }

            ForEach(model.agents) { agent in
                LabeledContent {
                    HStack(spacing: 8) {
                        Text(agent.agentType)
                        if !agent.model.isEmpty {
                            Text(agent.model)
                        }
                    }
                    .foregroundStyle(.secondary)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(agent.name)
                        if !agent.soul.isEmpty {
                            Text(agent.soul)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                }
                .contextMenu {
                    Button("common.edit") { editing = agent }
                    Button("common.delete", role: .destructive) {
                        Task { await model.delete(agent.id) }
                    }
                }
            }
        }
        .overlay {
            if model.agents.isEmpty {
                ContentUnavailableView(
                    "settings.agents.empty.title",
                    systemImage: "person.2",
                    description: Text("settings.agents.empty.description")
                )
            }
        }
        .navigationTitle("settings.agents")
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
            AgentEditor(existing: nil, engines: model.engines) { role in
                try await model.save(role)
            }
        }
        .sheet(item: $editing) { agent in
            AgentEditor(existing: agent, engines: model.engines) { role in
                try await model.save(role)
            }
        }
        .task { await model.load() }
    }
}
