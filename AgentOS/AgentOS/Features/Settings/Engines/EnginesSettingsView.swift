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
    @State private var isRefreshingSelected = false
    @State private var confirmRefreshSelected = false
    @SceneStorage("settings.engines.showUninstalled") private var showUninstalled = false

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
                        if showUninstalled {
                            ForEach(model.notInstalled) { engineRow($0) }
                        }
                    } header: {
                        Button {
                            showUninstalled.toggle()
                        } label: {
                            HStack(spacing: 5) {
                                Image(systemName: showUninstalled ? "chevron.down" : "chevron.right")
                                    .font(.caption2)
                                Text(verbatim: "\(String(localized: "settings.engines.missing")) (\(model.notInstalled.count))")
                                Spacer()
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .scrollIndicators(.hidden)
        } right: {
            detail
        }
        .navigationTitle("settings.engines")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    if envDirty {
                        confirmRefreshSelected = true
                    } else {
                        Task { await refreshSelected() }
                    }
                } label: {
                    if isRefreshingSelected {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("settings.engines.refreshSelected", systemImage: "arrow.clockwise")
                    }
                }
                .disabled(selected == nil || isRefreshingSelected || model.isLoading)
                .help("settings.engines.refreshSelected.help")
            }
        }
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
        .confirmationDialog(
            "settings.engines.refreshSelected.discard.title",
            isPresented: $confirmRefreshSelected,
            titleVisibility: .visible
        ) {
            Button("settings.engines.refreshSelected", role: .destructive) {
                Task { await refreshSelected() }
            }
            Button("common.cancel", role: .cancel) {}
        } message: {
            Text("settings.engines.refreshSelected.discard.message")
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
                HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(engine.name)
                            .font(.title3.weight(.semibold))
                        if !engine.summary.isEmpty {
                            Text(engine.summary)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }
                    }

                    Spacer(minLength: 12)

                    statusLabel(engine)
                }

                versionRow(engine)

                if !engine.available {
                    Text("settings.engines.platformUnsupported")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }

            runtimeSection(engine)
            envSection(engine)

            Section {
                diagnosticsGroup(engine)
            }
        }
        .formStyle(.grouped)
        .scrollIndicators(.hidden)
    }

    private func versionRow(_ engine: Engine) -> some View {
        HStack(spacing: 6) {
            Text("settings.engines.version")
                .foregroundStyle(.secondary)

            Spacer(minLength: 12)

            if model.installing == engine.agentType {
                ProgressView().controlSize(.mini)
                Text("settings.engines.installing")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else if !engine.isInstalled {
                Text(versionText(engine))
                    .monospacedDigit()
                    .foregroundStyle(.secondary)

                if engine.available {
                    Button("settings.engines.install") {
                        Task { await install(engine) }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                }
            } else if let latest = model.latest(for: engine) {
                Text(versionText(engine))
                    .monospacedDigit()

                Image(systemName: "arrow.right")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)

                Text(verbatim: "v\(latest)")
                    .monospacedDigit()
                    .foregroundStyle(.orange)

                Button("settings.engines.update") {
                    Task { await install(engine) }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            } else {
                Text(versionText(engine))
                    .monospacedDigit()

                if model.isCheckingLatest(engine) {
                    ProgressView().controlSize(.mini)
                } else if model.latestChecked.contains(engine.agentType) {
                    Text(
                        model.latestCheckFailed.contains(engine.agentType)
                            ? LocalizedStringKey("settings.engines.updateCheckFailed")
                            : LocalizedStringKey("settings.engines.upToDate")
                    )
                    .font(.callout)
                    .foregroundStyle(.secondary)
                }
            }
        }
    }

    private func runtimeSection(_ engine: Engine) -> some View {
        Section("settings.engines.runtime") {
            LabeledContent("settings.engines.localAvailable") {
                Text(availabilityText(engine))
                    .foregroundStyle(availabilityColor(engine))
            }

            LabeledContent("settings.engines.adapter") {
                Text(
                    engine.isAdapter
                        ? String(localized: "common.yes")
                        : String(localized: "common.no")
                )
            }

            LabeledContent("settings.engines.acpConnection") {
                Text(
                    engine.isInstalled
                        ? LocalizedStringKey("settings.engines.notDetected")
                        : LocalizedStringKey("settings.engines.missing")
                )
                .foregroundStyle(.secondary)
                .help("settings.engines.acpConnection.hint")
            }

            LabeledContent("settings.engines.installPath") {
                Text(verbatim: "—")
                    .foregroundStyle(.secondary)
                    .help("settings.engines.installPath.hint")
            }
        }
    }

    private func envSection(_ engine: Engine) -> some View {
        Section {
            ZStack(alignment: .topLeading) {
                TextEditor(text: $envText)
                    .font(.system(size: 12, design: .monospaced))
                    .scrollIndicators(.hidden)
                    .padding(6)
                    .frame(height: 150)
                    .background(Color(nsColor: .textBackgroundColor))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .overlay(
                        RoundedRectangle(cornerRadius: 6)
                            .stroke(.separator)
                    )

                if envText.isEmpty {
                    Text("settings.engines.env.empty")
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .padding(.horizontal, 11)
                        .padding(.vertical, 12)
                        .allowsHitTesting(false)
                }
            }

            if let error = parsedEnv.error {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        } header: {
            HStack(spacing: 8) {
                Text("settings.engines.env")

                Spacer()

                if envDirty {
                    Text("settings.engines.env.unsaved")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }

                Button("settings.engines.env.reset") {
                    syncEnv(engine, force: true)
                }
                .controlSize(.small)
                .disabled(!envDirty)

                Button("settings.engines.env.save") {
                    Task { await saveEnv(engine) }
                }
                .controlSize(.small)
                .disabled(!envDirty || parsedEnv.error != nil)
            }
        } footer: {
            Text("settings.engines.env.footer")
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func diagnosticsGroup(_ engine: Engine) -> some View {
        DisclosureGroup(isExpanded: $diagnosticsExpanded) {
            diagnosticsContent(engine)
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
    private func diagnosticsContent(_ engine: Engine) -> some View {
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
            HStack(spacing: 8) {
                Text(verbatim: String(
                    format: String(localized: "settings.engines.diagnostics.items"),
                    report.sections.reduce(0) { $0 + $1.checks.count }
                ))
                .font(.caption)
                .foregroundStyle(.secondary)

                Spacer()

                Button("settings.engines.diagnostics.rediagnose") {
                    Task { await model.loadDiagnostics(for: engine, force: true) }
                }
                .controlSize(.small)

                Button("settings.engines.diagnostics.copy") {
                    copyToPasteboard(report.plainText)
                }
                .controlSize(.small)
            }

            ForEach(sortedSections(report.sections)) { section in
                DiagnosticSectionRow(section: section)
            }
        } else {
            Text("settings.engines.diagnostics.expand")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func engineRow(_ engine: Engine) -> some View {
        HStack(spacing: 8) {
            Image(systemName: engine.isInstalled ? "checkmark.circle.fill" : "circle.dashed")
                .foregroundStyle(engine.isInstalled ? .green : .secondary)
                .font(.caption)
                .frame(width: 14)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(engine.name)
                    Spacer(minLength: 4)
                    rowAction(engine)
                }

                Text(rowCaption(engine))
                    .font(.caption)
                    .monospaced()
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .tag(engine.agentType)
    }

    @ViewBuilder
    private func rowAction(_ engine: Engine) -> some View {
        if model.installing == engine.agentType {
            ProgressView().controlSize(.mini)
        } else if engine.isInstalled {
            if model.latest(for: engine) != nil {
                Button {
                    Task { await install(engine) }
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .foregroundStyle(.orange)
                }
                .buttonStyle(.borderless)
                .help("settings.engines.update")
            }
        } else if engine.available {
            Button {
                Task { await install(engine) }
            } label: {
                Image(systemName: "arrow.down.circle")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.borderless)
            .help("settings.engines.install")
        }
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
        .font(.callout)
        .foregroundStyle(engine.isInstalled ? .green : .secondary)
    }

    private func availabilityText(_ engine: Engine) -> LocalizedStringKey {
        guard engine.isInstalled else {
            return "settings.engines.missing"
        }
        return engine.available
            ? "settings.engines.available"
            : "settings.engines.unavailable"
    }

    private func availabilityColor(_ engine: Engine) -> Color {
        guard engine.isInstalled else { return .secondary }
        return engine.available ? .green : .red
    }

    private func sortedSections(_ sections: [EngineDiagnosticsSection]) -> [EngineDiagnosticsSection] {
        sections.enumerated()
            .sorted { lhs, rhs in
                let left = sectionRank(lhs.element)
                let right = sectionRank(rhs.element)
                if left != right { return left < right }
                return lhs.offset < rhs.offset
            }
            .map(\.element)
    }

    private func sectionRank(_ section: EngineDiagnosticsSection) -> Int {
        switch worstStatus(section) {
        case "fail": 0
        case "warn": 1
        case "ok": 2
        default: 3
        }
    }

    private func verdictLabel(_ level: String) -> LocalizedStringKey {
        switch level {
        case "ok": "settings.engines.diagnostics.ok"
        case "warn": "settings.engines.diagnostics.warn"
        case "fail": "settings.engines.diagnostics.fail"
        default: "settings.engines.diagnostics.info"
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

    private func refreshSelected() async {
        guard let engine = selected else { return }

        isRefreshingSelected = true
        actionError = nil
        defer { isRefreshingSelected = false }

        await model.load()

        if let refreshed = model.engines.first(where: { $0.agentType == engine.agentType }) {
            syncEnv(refreshed, force: true)
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

private struct DiagnosticSectionRow: View {
    let section: EngineDiagnosticsSection

    @State private var expanded: Bool

    init(section: EngineDiagnosticsSection) {
        self.section = section
        _expanded = State(initialValue: worstStatus(section) != "ok")
    }

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            ForEach(section.checks) { check in
                VStack(alignment: .leading, spacing: 3) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(check.label)
                        Spacer(minLength: 8)
                        Text(check.value)
                            .font(.caption)
                            .monospaced()
                            .textSelection(.enabled)

                        if check.status != "ok" {
                            Image(systemName: statusSymbol(check.status))
                                .font(.caption)
                                .foregroundStyle(statusColor(check.status))
                        }
                    }

                    if check.status != "ok", let hint = check.hint, !hint.isEmpty {
                        Text(hint)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(.vertical, 1)
            }
        } label: {
            HStack(spacing: 8) {
                Text(section.title)
                Spacer(minLength: 8)
                Text(summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(verdictLabel(worstStatus(section)))
                    .font(.caption)
                    .foregroundStyle(statusColor(worstStatus(section)))
            }
        }
    }

    private var summary: String {
        let issue = section.checks.first {
            $0.status != "ok" && !$0.value.isEmpty
        }
        return issue?.value
            ?? section.checks.first(where: { !$0.value.isEmpty })?.value
            ?? ""
    }

    private func verdictLabel(_ level: String) -> LocalizedStringKey {
        switch level {
        case "ok": "settings.engines.diagnostics.ok"
        case "warn": "settings.engines.diagnostics.warn"
        case "fail": "settings.engines.diagnostics.fail"
        default: "settings.engines.diagnostics.info"
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

    private func statusSymbol(_ level: String) -> String {
        switch level {
        case "warn": "exclamationmark.triangle.fill"
        case "fail": "xmark.octagon.fill"
        default: "info.circle"
        }
    }
}

private func worstStatus(_ section: EngineDiagnosticsSection) -> String {
    if section.checks.contains(where: { $0.status == "fail" }) { return "fail" }
    if section.checks.contains(where: { $0.status == "warn" }) { return "warn" }
    if section.checks.contains(where: { $0.status == "ok" }) { return "ok" }
    return "info"
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
