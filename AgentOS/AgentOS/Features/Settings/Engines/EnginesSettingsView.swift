import SwiftUI

struct EnginesSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?

    private var model: EnginesSettingsViewModel { environment.engines }

    private var selected: Engine? {
        guard let selection else { return nil }
        return model.engines.first { $0.agentType == selection }
    }

    var body: some View {
        MasterDetailLayout {
            List(model.engines, selection: $selection) { engine in
                VStack(alignment: .leading, spacing: 2) {
                    Text(engine.name)
                    Text(engine.installedVersion ?? "—")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .tag(engine.agentType)
            }
        } right: {
            detail
        }
        .navigationTitle("settings.engines")
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
        } else if let engine = selected {
            engineDetail(engine)
        } else if model.engines.isEmpty {
            ContentUnavailableView(
                "settings.engines.empty.title",
                systemImage: "terminal",
                description: Text("settings.engines.empty.description")
            )
        } else {
            SelectItemPlaceholder()
        }
    }

    private func engineDetail(_ engine: Engine) -> some View {
        Form {
            Section("settings.engines.info") {
                LabeledContent("settings.engines.available") {
                    Label(
                        engine.available ? "common.yes" : "common.no",
                        systemImage: engine.available ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                    )
                    .foregroundStyle(engine.available ? .green : .orange)
                }
                LabeledContent("settings.engines.version", value: engine.installedVersion ?? "—")
                LabeledContent("settings.engines.distribution", value: engine.distributionType)
                LabeledContent("settings.engines.adapter") {
                    Text(engine.isAdapter ? String(localized: "common.yes") : String(localized: "common.no"))
                }
            }

            Section {
                Toggle(
                    "settings.engines.enabled",
                    isOn: Binding(
                        get: { engine.enabled },
                        set: { value in
                            Task { await model.setEnabled(engine, enabled: value) }
                        }
                    )
                )
            } header: {
                Text("settings.engines.behavior")
            } footer: {
                if !engine.summary.isEmpty {
                    Text(engine.summary).foregroundStyle(.secondary)
                }
            }

            if !engine.env.isEmpty {
                Section("settings.engines.env") {
                    ForEach(engine.env.sorted(by: { $0.key < $1.key }), id: \.key) { pair in
                        LabeledContent(pair.key) {
                            Text(pair.value)
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
    }

    private func selectFirstIfNeeded() {
        guard let selection, model.engines.contains(where: { $0.agentType == selection }) else {
            self.selection = model.engines.first?.agentType
            return
        }
    }
}
