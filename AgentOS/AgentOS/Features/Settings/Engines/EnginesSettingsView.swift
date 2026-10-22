import AppKit
import SwiftUI

struct EnginesSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var envText = ""
    @State private var envEngineID: String?
    @State private var actionError: String?
    @State private var pendingSelection: String?
    @State private var diagnosticsExpanded = false

    private var model: EnginesSettingsViewModel { environment.engines }

    private var selected: Engine? {
        guard let selection else { return nil }
        return model.engines.first { $0.agentType == selection }
    }

    var body: some View {
        MasterDetailLayout {
            List(selection: listSelection) {
                if !model.installed.isEmpty {
                    Section {
                        ForEach(model.installed) { engineRow($0) }
                    } header: {
                        Text(verbatim: "\(String(localized: "settings.engines.installed")) (\(model.installed.count))")
                    }
                }

                if !model.notInstalled.isEmpty {
                    Section {
                        ForEach(model.notInstalled) { engineRow($0) }
                    } header: {
                        Text(verbatim: "\(String(localized: "settings.engines.missing")) (\(model.notInstalled.count))")
                    }
                }
            }
        } right: {
            detail
        }
        .navigationTitle("settings.engines")
        .task {
            await model.load()
            selectFirstIfNeeded()
            if let selected {
                syncEnv(selected, force: true)
                await model.checkLatest(for: selected)
            }
        }
        .onChange(of: selection) { _, _ in
            actionError = nil
            model.clearInstallError()
            diagnosticsExpanded = false

            guard let selected else {
                envText = ""
                envEngineID = nil
                return
            }

            syncEnv(selected)
            Task { await model.checkLatest(for: selected) }
        }
        .confirmationDialog(
            "settings.engines.env.discard.title",
            isPresented: Binding(
                get: { pendingSelection != nil },
                set: { if !$0 { pendingSelection = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("settings.engines.env.discard.confirm", role: .destructive) {
                selection = pendingSelection
                pendingSelection = nil
            }
            Button("common.cancel", role: .cancel) {
                pendingSelection = nil
            }
        } message: {
            Text("settings.engines.env.discard.message")
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
                    Task { await model.load() }
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
                Button("settings.engines.retry") {
                    Task { await model.load() }
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
                    Text(actionError).foregroundStyle(.red)
                }
            }

            if let installError = model.installError {
                Section {
                    Text(installError).foregroundStyle(.red)
                }
            }

            Section {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Text(engine.name)
                            .font(.headline)
                        Spacer()
                        statusLabel(engine)
                    }
                    if !engine.summary.isEmpty {
                        Text(engine.summary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }

                LabeledContent("settings.engines.version") {
                    HStack(spacing: 6) {
                        Text(versionText(engine))
                            .monospacedDigit()
                            .textSelection(.enabled)

                        if model.isCheckingLatest(engine) {
                            ProgressView().controlSize(.mini)
                        }
                    }
                }

                if let latest = model.latest(for: engine) {
                    LabeledContent("settings.engines.updateAvailable") {
                        Text(verbatim: "v\(latest)")
                            .monospacedDigit()
                            .foregroundStyle(.orange)
                    }
                }

                HStack(spacing: 8) {
                    if model.installing == engine.agentType {
                        ProgressView().controlSize(.small)
                        Text("settings.engines.installing")
                            .foregroundStyle(.secondary)
                    } else {
                        if !engine.isInstalled || model.latest(for: engine) != nil {
                            Button {
                                Task { await install(engine) }
                            } label: {
                                Label(installTitle(engine), systemImage: "arrow.down.circle")
                            }
                            .buttonStyle(.borderedProminent)
                            .disabled(!engine.available || model.installing != nil)
                        } else if model.latestChecked.contains(engine.agentType) {
                            Text(
                                model.latestCheckFailed.contains(engine.agentType)
                                    ? LocalizedStringKey("settings.engines.updateCheckFailed")
                                    : LocalizedStringKey("settings.engines.upToDate")
                            )
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        } else if model.isCheckingLatest(engine) {
                            Text("settings.engines.checkingUpdate")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }

                        Button("settings.engines.refresh") {
                            Task { await refresh(engine) }
                        }
                        .disabled(model.isLoading || model.installing != nil)
                    }

                    Spacer()
                }

                if !engine.available {
                    Text("settings.engines.platformUnsupported")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }

            envSection(engine)

            Section {
                diagnosticsGroup(engine)
            }
        }
        .formStyle(.grouped)
    }

    private func envSection(_ engine: Engine) -> some View {
        Section {
            TextEditor(text: $envText)
                .font(.system(.caption, design: .monospaced))
                .frame(minHeight: 120, maxHeight: 180)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(.separator)
                )

            if let error = parsedEnv.error {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            Text("settings.engines.env.formatHint")
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack(spacing: 8) {
                if envDirty {
                    Text("settings.engines.env.unsaved")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }

                Spacer()

                Button("settings.engines.env.save") {
                    Task { await saveEnv(engine) }
                }
                .disabled(!envDirty || parsedEnv.error != nil)
            }
        } header: {
            Text("settings.engines.env")
        } footer: {
            Text("settings.engines.env.footer")
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func diagnosticsGroup(_ engine: Engine) -> some View {
        DisclosureGroup(isExpanded: $diagnosticsExpanded) {
            if model.diagnosticsLoading.contains(engine.agentType) {
                ProgressView().controlSize(.small)
            } else if let error = model.diagnosticsError[engine.agentType] {
                VStack(alignment: .leading, spacing: 6) {
                    Text(error).foregroundStyle(.red)
                    Button("settings.engines.retry") {
                        Task { await model.loadDiagnostics(for: engine, force: true) }
                    }
                    .buttonStyle(.link)
                    .font(.caption)
                }
            } else if let report = model.diagnostics[engine.agentType] {
                ForEach(report.sections) { section in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(section.title)
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)

                        ForEach(section.checks) { check in
                            HStack(alignment: .firstTextBaseline, spacing: 8) {
                                Text(check.label)
                                Spacer(minLength: 8)
                                Text(check.value)
                                    .font(.caption)
                                    .monospaced()
                                    .textSelection(.enabled)
                                Image(systemName: statusSymbol(check.status))
                                    .foregroundStyle(statusColor(check.status))
                                    .font(.caption)
                            }

                            if let hint = check.hint, !hint.isEmpty {
                                Text(hint)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }

                Button("settings.engines.diagnostics.copy") {
                    copyToPasteboard(report.plainText)
                }
                .controlSize(.small)
            } else {
                Text("settings.engines.diagnostics.expand")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        } label: {
            HStack(spacing: 8) {
                Text("settings.engines.diagnostics")

                Spacer()

                if let report = model.diagnostics[engine.agentType] {
                    Text(verdictLabel(report.level))
                        .font(.caption)
                        .foregroundStyle(statusColor(report.level))
                } else if model.diagnosticsLoading.contains(engine.agentType) {
                    ProgressView().controlSize(.mini)
                }
            }
        }
        .onChange(of: diagnosticsExpanded) { _, expanded in
            guard expanded else { return }
            Task { await model.loadDiagnostics(for: engine) }
        }
    }

    @ViewBuilder
    private func engineRow(_ engine: Engine) -> some View {
        HStack(spacing: 8) {
            Image(systemName: engine.isInstalled ? "checkmark.circle.fill" : "circle.dashed")
                .foregroundStyle(engine.isInstalled ? .green : .secondary)
                .font(.caption)

            VStack(alignment: .leading, spacing: 2) {
                Text(engine.name)
                Text(rowCaption(engine))
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 4)

            if model.latest(for: engine) != nil {
                Image(systemName: "arrow.down.circle.fill")
                    .foregroundStyle(.orange)
                    .font(.caption)
                    .help("settings.engines.updateAvailable")
            }
        }
        .tag(engine.agentType)
    }

    private func rowCaption(_ engine: Engine) -> String {
        if !engine.available {
            return String(localized: "settings.engines.platformUnsupported")
        }
        guard let version = engine.installedVersion else {
            return String(localized: "settings.engines.missing")
        }
        return "v\(version)"
    }

    private func versionText(_ engine: Engine) -> String {
        guard let version = engine.installedVersion else {
            return String(localized: "settings.engines.missing")
        }
        return "v\(version)"
    }

    private func statusLabel(_ engine: Engine) -> some View {
        Label(
            engine.isInstalled
                ? LocalizedStringKey("settings.engines.installed")
                : LocalizedStringKey("settings.engines.missing"),
            systemImage: engine.isInstalled ? "checkmark.circle.fill" : "circle.dashed"
        )
        .foregroundStyle(engine.isInstalled ? .green : .secondary)
        .font(.caption)
    }

    private func installTitle(_ engine: Engine) -> String {
        if let latest = model.latest(for: engine) {
            return String(
                format: String(localized: "settings.engines.updateTo"),
                latest
            )
        }
        return String(localized: "settings.engines.install")
    }

    private func verdictLabel(_ level: String) -> LocalizedStringKey {
        switch level {
        case "ok": "settings.engines.diagnostics.ok"
        case "warn": "settings.engines.diagnostics.warn"
        case "fail": "settings.engines.diagnostics.fail"
        default: "settings.engines.diagnostics.info"
        }
    }

    private func statusSymbol(_ level: String) -> String {
        switch level {
        case "ok": "checkmark.circle.fill"
        case "warn": "exclamationmark.triangle.fill"
        case "fail": "xmark.octagon.fill"
        default: "info.circle"
        }
    }

    private func statusColor(_ level: String) -> Color {
        switch level {
        case "ok": .green
        case "warn": .orange
        case "fail": .red
        default: .secondary
        }
    }

    private var listSelection: Binding<String?> {
        Binding(
            get: { selection },
            set: { newValue in
                guard let newValue, newValue != selection else { return }
                if envDirty {
                    pendingSelection = newValue
                } else {
                    selection = newValue
                }
            }
        )
    }

    private var parsedEnv: EnvParseResult {
        EnvParser.parse(envText)
    }

    private var envDirty: Bool {
        guard let selected, envEngineID == selected.agentType else { return false }
        return parsedEnv.values != selected.env
    }

    private func syncEnv(_ engine: Engine, force: Bool = false) {
        guard force || envEngineID != engine.agentType else { return }
        envText = engine.env.keys.sorted()
            .map { "\($0)=\(engine.env[$0] ?? "")" }
            .joined(separator: "\n")
        envEngineID = engine.agentType
    }

    private func install(_ engine: Engine) async {
        actionError = nil
        await model.install(engine, version: model.latest(for: engine))

        if let refreshed = model.engines.first(where: { $0.agentType == engine.agentType }) {
            await model.checkLatest(for: refreshed, force: true)
        }
    }

    private func refresh(_ engine: Engine) async {
        actionError = nil
        let hadUnsavedEnv = envDirty
        await model.load()

        if let refreshed = model.engines.first(where: { $0.agentType == engine.agentType }) {
            if !hadUnsavedEnv {
                syncEnv(refreshed, force: true)
            }
            await model.checkLatest(for: refreshed, force: true)
        }
    }

    private func saveEnv(_ engine: Engine) async {
        actionError = nil
        let parsed = EnvParser.parse(envText)
        guard parsed.error == nil else { return }

        do {
            try await model.update(engine, enabled: engine.enabled, env: parsed.values)
        } catch {
            actionError = error.localizedDescription
        }
    }

    private func selectFirstIfNeeded() {
        guard let selection, model.engines.contains(where: { $0.agentType == selection }) else {
            self.selection = model.installed.first?.agentType ?? model.engines.first?.agentType
            return
        }
    }

    private func copyToPasteboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

private struct EnvParseResult {
    var values: [String: String] = [:]
    var error: String?
}

private enum EnvParser {
    static func parse(_ text: String) -> EnvParseResult {
        var result = EnvParseResult()
        let normalized = text
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")

        for (index, rawLine) in normalized
            .split(separator: "\n", omittingEmptySubsequences: false)
            .enumerated() {
            var line = String(rawLine).trimmingCharacters(in: .whitespaces)
            if line.isEmpty || line.hasPrefix("#") { continue }
            if line.hasPrefix("export ") {
                line = String(line.dropFirst("export ".count))
            }

            guard let separator = line.firstIndex(of: "=") else {
                result.error = invalidLine(index + 1)
                continue
            }

            let key = String(line[..<separator])
                .trimmingCharacters(in: .whitespaces)
            let value = String(line[line.index(after: separator)...])

            guard isValidKey(key) else {
                result.error = invalidLine(index + 1)
                continue
            }

            result.values[key] = value
        }

        return result
    }

    private static func isValidKey(_ key: String) -> Bool {
        guard let first = key.first, first == "_" || first.isLetter else {
            return false
        }
        return key.dropFirst().allSatisfy {
            $0 == "_" || $0.isLetter || $0.isNumber
        }
    }

    private static func invalidLine(_ line: Int) -> String {
        String(
            format: String(localized: "settings.engines.env.invalidLine"),
            line
        )
    }
}
