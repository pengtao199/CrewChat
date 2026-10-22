import SwiftUI

struct EnginesSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var env: [String: String] = [:]
    @State private var envEngineID: String?
    @State private var newKey = ""
    @State private var newValue = ""
    @State private var actionError: String?
    @State private var pendingDisable: DisableRequest?

    private var model: EnginesSettingsViewModel { environment.engines }

    private var selected: Engine? {
        guard let selection else { return nil }
        return model.engines.first { $0.agentType == selection }
    }

    var body: some View {
        MasterDetailLayout {
            List(model.engines, selection: $selection) { engine in
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(engine.name)
                        if !engine.available {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundStyle(.orange)
                                .font(.caption)
                        }
                    }
                    Text(engine.installedVersion ?? String(localized: "settings.engines.missing"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .tag(engine.agentType)
            }
        } right: {
            detail
        }
        .navigationTitle("settings.engines")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await refresh() }
                } label: {
                    Label("settings.engines.refresh", systemImage: "arrow.clockwise")
                }
                .disabled(model.isLoading)
            }
        }
        .onChange(of: selection) { _, _ in
            actionError = nil
            if let selected {
                syncEnv(selected)
            }
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
            if let selected {
                syncEnv(selected, force: true)
            }
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let message = model.errorMessage {
            ContentUnavailableView {
                Label("common.error", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                Button("settings.engines.retry") {
                    Task { await refresh() }
                }
            }
        } else if let engine = selected {
            engineDetail(engine)
        } else if model.engines.isEmpty {
            ContentUnavailableView {
                Label("settings.engines.empty.title", systemImage: "terminal")
            } description: {
                Text("settings.engines.empty.description")
            } actions: {
                Button("settings.engines.refresh") {
                    Task { await refresh() }
                }
            }
        } else {
            SelectItemPlaceholder()
        }
    }

    private func engineDetail(_ engine: Engine) -> some View {
        Form {
            if let actionError {
                Section {
                    Text(actionError)
                        .foregroundStyle(.red)
                }
            }

            Section("settings.engines.info") {
                LabeledContent("settings.engines.available") {
                    Label(
                        engine.available ? "common.yes" : "common.no",
                        systemImage: engine.available ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                    )
                    .foregroundStyle(engine.available ? .green : .orange)
                }
                LabeledContent("settings.engines.version") {
                    Text(engine.installedVersion ?? "—")
                        .textSelection(.enabled)
                }
                LabeledContent("settings.engines.distribution", value: engine.distributionType)
                LabeledContent("settings.engines.adapter") {
                    Text(engine.isAdapter ? String(localized: "common.yes") : String(localized: "common.no"))
                }
                LabeledContent("settings.engines.usedBy", value: "\(model.agentsUsing(engine.agentType).count)")
            }

            if !usingAgents.isEmpty {
                Section {
                    Text(
                        String(
                            format: String(localized: "settings.engines.usedByList"),
                            usingAgents.map(\.name).joined(separator: "、")
                        )
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }

            if !engine.available {
                Section {
                    Text("settings.engines.unavailableHint")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }

            Section {
                Toggle(
                    "settings.engines.enabled",
                    isOn: Binding(
                        get: { engine.enabled },
                        set: { value in
                            let using = model.agentsUsing(engine.agentType)
                            if !value, !using.isEmpty {
                                pendingDisable = DisableRequest(
                                    engine: engine,
                                    roleNames: using.map(\.name)
                                )
                                return
                            }
                            Task { await setEnabled(engine, value) }
                        }
                    )
                )
                .disabled(!engine.available)
            } header: {
                Text("settings.engines.behavior")
            } footer: {
                if !engine.summary.isEmpty {
                    Text(engine.summary).foregroundStyle(.secondary)
                }
            }

            Section {
                if env.isEmpty {
                    Text("settings.engines.env.empty")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(env.keys.sorted(), id: \.self) { key in
                        LabeledContent {
                            HStack(spacing: 8) {
                                TextField(
                                    "",
                                    text: Binding(
                                        get: { env[key] ?? "" },
                                        set: { env[key] = $0 }
                                    ),
                                    prompt: Text("settings.engines.env.value")
                                )
                                .labelsHidden()
                                .textFieldStyle(.roundedBorder)

                                Button {
                                    env.removeValue(forKey: key)
                                } label: {
                                    Image(systemName: "minus.circle")
                                }
                                .buttonStyle(.borderless)
                                .help("common.delete")
                            }
                        } label: {
                            Text(key)
                                .font(.system(.caption, design: .monospaced))
                                .textSelection(.enabled)
                        }
                    }
                }

                VStack(alignment: .leading, spacing: 6) {
                    Text("settings.engines.env.add")
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    HStack(spacing: 8) {
                        TextField(
                            "",
                            text: $newKey,
                            prompt: Text("settings.engines.env.key")
                        )
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)

                        TextField(
                            "",
                            text: $newValue,
                            prompt: Text("settings.engines.env.value")
                        )
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)

                        Button {
                            addEnv()
                        } label: {
                            Image(systemName: "plus.circle")
                        }
                        .buttonStyle(.borderless)
                        .disabled(newKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }

                HStack {
                    if envDirty {
                        Text("settings.engines.env.unsaved")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    Spacer()
                    Button("settings.engines.env.save") {
                        Task { await saveEnv(engine) }
                    }
                    .disabled(!envDirty || model.isLoading)
                }
            } header: {
                Text("settings.engines.env")
            } footer: {
                Text("settings.engines.env.footer")
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .alert(
            "settings.engines.disable.title",
            isPresented: Binding(
                get: { pendingDisable != nil },
                set: { if !$0 { pendingDisable = nil } }
            ),
            presenting: pendingDisable
        ) { request in
            Button("settings.engines.disable.confirm", role: .destructive) {
                pendingDisable = nil
                Task { await setEnabled(request.engine, false) }
            }
            Button("common.cancel", role: .cancel) {
                pendingDisable = nil
            }
        } message: { request in
            Text(
                String(
                    format: String(localized: "settings.engines.disable.message"),
                    request.roleNames.joined(separator: "、")
                )
            )
        }
    }

    private var usingAgents: [AgentRole] {
        guard let selected else { return [] }
        return model.agentsUsing(selected.agentType)
    }

    private var envDirty: Bool {
        guard let selected else { return false }
        return env != selected.env
    }

    private func syncEnv(_ engine: Engine, force: Bool = false) {
        guard force || envEngineID != engine.agentType else { return }
        env = engine.env
        envEngineID = engine.agentType
        newKey = ""
        newValue = ""
    }

    private func addEnv() {
        let key = newKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty else { return }
        env[key] = newValue
        newKey = ""
        newValue = ""
    }

    private func refresh() async {
        await model.load()
        if let selected {
            syncEnv(selected)
        }
    }

    private func setEnabled(_ engine: Engine, _ value: Bool) async {
        actionError = nil
        do {
            try await model.update(engine, enabled: value, env: engine.env)
        } catch {
            actionError = error.localizedDescription
            await model.load()
        }
    }

    private func saveEnv(_ engine: Engine) async {
        actionError = nil
        do {
            try await model.update(engine, enabled: engine.enabled, env: env)
            if let refreshed = model.engines.first(where: { $0.agentType == engine.agentType }) {
                syncEnv(refreshed, force: true)
            }
        } catch {
            actionError = error.localizedDescription
        }
    }

    private func selectFirstIfNeeded() {
        guard let selection, model.engines.contains(where: { $0.agentType == selection }) else {
            self.selection = model.engines.first?.agentType
            return
        }
    }
}

private struct DisableRequest: Identifiable {
    let engine: Engine
    let roleNames: [String]

    var id: String { engine.agentType }
}
