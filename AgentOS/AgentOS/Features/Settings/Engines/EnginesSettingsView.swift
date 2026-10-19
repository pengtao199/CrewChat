import SwiftUI

struct EnginesSettingsView: View {
    @Environment(AppEnvironment.self) private var environment

    private var model: EnginesSettingsViewModel { environment.engines }

    var body: some View {
        List {
            if let message = model.errorMessage {
                Text(message).foregroundStyle(.red)
            }

            ForEach(model.engines) { engine in
                LabeledContent {
                    HStack(spacing: 12) {
                        Text(engine.installedVersion ?? "—")
                            .foregroundStyle(.secondary)
                            .monospacedDigit()

                        Label(
                            engine.available ? "settings.engines.available" : "settings.engines.missing",
                            systemImage: engine.available ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                        )
                        .labelStyle(.iconOnly)
                        .foregroundStyle(engine.available ? .green : .orange)

                        Toggle(
                            "settings.engines.enabled",
                            isOn: Binding(
                                get: { engine.enabled },
                                set: { value in
                                    Task { await model.setEnabled(engine, enabled: value) }
                                }
                            )
                        )
                        .labelsHidden()
                    }
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(engine.name)
                        if !engine.summary.isEmpty {
                            Text(engine.summary)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                }
            }
        }
        .overlay {
            if model.engines.isEmpty {
                ContentUnavailableView(
                    "settings.engines.empty.title",
                    systemImage: "terminal",
                    description: Text("settings.engines.empty.description")
                )
            }
        }
        .navigationTitle("settings.engines")
        .task { await model.load() }
    }
}
