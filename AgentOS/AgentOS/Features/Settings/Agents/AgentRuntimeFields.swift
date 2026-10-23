import SwiftUI

struct AgentRuntimeFields: View {
    @Binding var role: AgentRole

    let engines: [Engine]
    let enginesError: String?
    let face: ModelFace?
    let faceError: String?
    let isLoadingFace: Bool
    let onRetryFace: () -> Void

    var body: some View {
        engineField

        if let enginesError {
            Text(enginesError)
                .font(.caption)
                .foregroundStyle(.red)
        }

        modelFields
            .onChange(of: face) { _, _ in
                normalizeModelSelection()
            }
            .onAppear {
                normalizeModelSelection()
            }
    }

    @ViewBuilder
    private var engineField: some View {
        if runnableEngines.isEmpty {
            Text("settings.agents.engine.noneAvailable")
                .font(.callout)
                .foregroundStyle(.secondary)
        } else {
            Picker("settings.agents.engine", selection: $role.agentType) {
                if role.agentType.isEmpty {
                    Text("settings.agents.engine.none").tag("")
                }
                ForEach(runnableEngines) { engine in
                    Text(engine.name).tag(engine.agentType)
                }
            }

            if !role.agentType.isEmpty,
               !runnableEngines.contains(where: { $0.agentType == role.agentType }) {
                Text("settings.agents.engine.unavailableCurrent")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
    }

    @ViewBuilder
    private var modelFields: some View {
        if isLoadingFace {
            ProgressView().controlSize(.small)
        } else if let faceError {
            errorLine(faceError, retry: onRetryFace)
        } else if let face {
            if face.supported {
                providerField
                modelField

                if let hint = faceHint {
                    Text(hint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } else if let hint = faceHint {
                Text(hint)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var providerField: some View {
        Picker("settings.agents.provider", selection: providerBinding) {
            if role.modelProvider.isEmpty {
                Text("settings.agents.model.default").tag("")
            }
            ForEach(providerChoices) { choice in
                Text(choice.name).tag(choice.id)
            }
        }
        .disabled(providerChoices.isEmpty)
    }

    private var modelField: some View {
        VStack(alignment: .leading, spacing: 4) {
            Picker("settings.agents.model", selection: $role.model) {
                Text("settings.agents.model.default").tag("")
                ForEach(modelOptions, id: \.self) { name in
                    Text(name).tag(name)
                }
            }
            .disabled(modelOptions.isEmpty)

            if let face, face.supported, !role.model.isEmpty, !modelOptions.contains(role.model) {
                HStack(spacing: 6) {
                    Label("settings.agents.modelInvalid", systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                    Button("settings.agents.modelClear") {
                        role.model = ""
                    }
                    .buttonStyle(.link)
                    .font(.caption)
                }
            }
        }
    }

    private var runnableEngines: [Engine] {
        engines.filter(\.available)
    }

    private var providerChoices: [ProviderChoice] {
        var seen = Set<String>()
        var result: [ProviderChoice] = []

        for model in face?.models ?? [] {
            let id = model.providerID.isEmpty ? model.provider : model.providerID
            guard seen.insert(id).inserted else { continue }
            result.append(ProviderChoice(id: id, name: model.provider))
        }

        return result
    }

    private var resolvedProviderID: String {
        if providerChoices.contains(where: { $0.id == role.modelProvider }) {
            return role.modelProvider
        }
        if let choice = providerChoices.first(where: { $0.name == role.modelProvider }) {
            return choice.id
        }
        return role.modelProvider
    }

    private var providerBinding: Binding<String> {
        Binding(
            get: { resolvedProviderID },
            set: { providerID in
                role.modelProvider = providerID
                if !modelOptions(for: providerID).contains(role.model) {
                    role.model = ""
                }
            }
        )
    }

    private var modelOptions: [String] {
        modelOptions(for: resolvedProviderID)
    }

    private func modelOptions(for providerID: String) -> [String] {
        (face?.models ?? [])
            .filter { ($0.providerID.isEmpty ? $0.provider : $0.providerID) == providerID }
            .map(\.model)
    }

    private var faceHint: String? {
        guard let face else { return nil }
        if !face.supported {
            return String(localized: "settings.agents.modelUnsupported")
        }
        if face.models.isEmpty {
            return face.reason ?? String(localized: "settings.agents.modelUnsupported")
        }
        return nil
    }

    private func normalizeModelSelection() {
        guard let face else { return }

        if !face.supported {
            role.modelProvider = ""
            role.model = ""
            return
        }

        let providers = providerChoices

        if !providers.contains(where: { $0.id == resolvedProviderID }) {
            role.modelProvider = providers.first?.id ?? ""
        } else if role.modelProvider != resolvedProviderID {
            role.modelProvider = resolvedProviderID
        }

        if !role.model.isEmpty, !modelOptions.contains(role.model) {
            role.model = ""
        }
    }

    private func errorLine(_ message: String, retry: @escaping () -> Void) -> some View {
        HStack(spacing: 6) {
            Text(message)
                .font(.caption)
                .foregroundStyle(.red)
            Button("settings.agents.retry", action: retry)
                .buttonStyle(.link)
                .font(.caption)
        }
    }
}

private struct ProviderChoice: Identifiable, Hashable {
    let id: String
    let name: String
}
