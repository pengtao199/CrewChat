import AppKit
import SwiftUI

struct SkillsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var query = ""
    @State private var showInstall = false
    @State private var confirmRemove = false

    private var model: SkillsSettingsViewModel { environment.skills }

    var body: some View {
        MasterDetailLayout {
            sidebar
        } right: {
            detail
        }
        .navigationTitle("settings.skills")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    showInstall = true
                } label: {
                    Label("settings.skills.add", systemImage: "plus")
                }
                .disabled(model.isUpdating || model.isRemoving)
                .help("settings.skills.add")

                Button {
                    Task { await model.refresh() }
                } label: {
                    if model.isLoading || model.isCheckingUpdate {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("settings.skills.refresh", systemImage: "arrow.clockwise")
                    }
                }
                .disabled(
                    model.isLoading
                        || model.isCheckingUpdate
                        || model.isUpdating
                        || model.isRemoving
                )
                .help("settings.skills.refresh")

                Button(role: .destructive) {
                    confirmRemove = true
                } label: {
                    Label("settings.skills.remove.action", systemImage: "trash")
                }
                .disabled(model.selected == nil || model.isUpdating || model.isRemoving)
                .help("settings.skills.remove.action")
            }
        }
        .sheet(isPresented: $showInstall) {
            SkillInstallSheet { request in
                try await model.install(request)
            }
        }
        .confirmationDialog(
            "settings.skills.remove.title",
            isPresented: $confirmRemove,
            presenting: model.selected
        ) { skill in
            Button("settings.skills.remove.action", role: .destructive) {
                Task { await model.remove(skill) }
            }
            Button("common.cancel", role: .cancel) {}
        } message: { skill in
            Text(
                String(
                    format: String(localized: "settings.skills.remove.message"),
                    skill.name
                )
            )
        }
        .task {
            await model.load()
        }
    }

    private var sidebar: some View {
        VStack(spacing: 0) {
            TextField(
                "",
                text: $query,
                prompt: Text("settings.skills.search")
            )
            .labelsHidden()
            .textFieldStyle(.roundedBorder)
            .padding(10)

            Divider()

            if model.isLoading, model.skills.isEmpty {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if filteredSkills.isEmpty {
                VStack(spacing: 4) {
                    Text("settings.skills.empty.title")
                        .font(.callout)
                    Text(query.isEmpty
                        ? LocalizedStringKey("settings.skills.empty.description")
                        : LocalizedStringKey("settings.skills.noMatch"))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity)
                .padding(20)
                .frame(maxHeight: .infinity, alignment: .center)
            } else {
                List(selection: selectionBinding) {
                    ForEach(filteredSkills) { skill in
                        row(skill)
                            .tag(skill.id)
                    }
                }
                .scrollIndicators(.hidden)
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
                Button("settings.skills.retry") {
                    Task { await model.load() }
                }
            }
        } else if let skill = model.selected {
            skillDetail(skill)
        } else if model.isLoading {
            ProgressView()
                .controlSize(.small)
        } else {
            SelectItemPlaceholder()
        }
    }

    private func skillDetail(_ skill: Skill) -> some View {
        Form {
            if let actionError = model.actionError {
                Section {
                    Text(actionError).foregroundStyle(.red)
                }
            }

            Section {
                HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(skill.name)
                            .font(.title3.weight(.semibold))

                        if let description = skill.description, !description.isEmpty {
                            Text(description)
                                .font(.callout)
                                .foregroundStyle(.secondary)
                                .textSelection(.enabled)
                        }
                    }

                    Spacer(minLength: 12)
                }

                LabeledContent("settings.skills.id") {
                    Text(skill.id)
                        .font(.callout)
                        .monospaced()
                        .textSelection(.enabled)
                }
            }

            if hasMetadata(skill) {
                metadataSection(skill)
            }

            if !model.files.isEmpty {
                filesSection
            }

            Section {
                contentSection(skill)
            } header: {
                Text("settings.skills.content")
            }
        }
        .formStyle(.grouped)
        .scrollIndicators(.hidden)
    }

    private func metadataSection(_ skill: Skill) -> some View {
        Section("settings.skills.sourceSection") {
            if let source = sourceText(skill) {
                LabeledContent("settings.skills.source") {
                    Text(source)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
            }

            if let ref = skill.sourceRef, !ref.isEmpty {
                LabeledContent("settings.skills.source.ref") {
                    Text(ref)
                        .font(.caption)
                        .monospaced()
                        .textSelection(.enabled)
                }
            }

            if let subdir = skill.sourceSubdir, !subdir.isEmpty {
                LabeledContent("settings.skills.source.subdir") {
                    Text(subdir)
                        .font(.caption)
                        .monospaced()
                        .textSelection(.enabled)
                }
            }

            if let hash = skill.currentHash, !hash.isEmpty {
                LabeledContent("settings.skills.hash") {
                    HStack(spacing: 6) {
                        Text(shortHash(hash))
                            .font(.caption)
                            .monospaced()
                            .textSelection(.enabled)
                            .help(hash)

                        if model.isUpdating {
                            ProgressView().controlSize(.mini)
                        } else if let update = model.update, update.updateAvailable {
                            Image(systemName: "arrow.right")
                                .font(.caption2)
                                .foregroundStyle(.tertiary)

                            Text(shortHash(update.latestHash))
                                .font(.caption)
                                .monospaced()
                                .foregroundStyle(.orange)

                            Button("settings.skills.update") {
                                Task { await model.applyUpdate(skill) }
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                        } else if model.isCheckingUpdate {
                            ProgressView().controlSize(.mini)
                        } else if let error = model.updateCheckError {
                            Text("settings.skills.updateCheckFailed")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .help(error)
                        }
                    }
                }
            }

            if let license = skill.license, !license.isEmpty {
                LabeledContent("settings.skills.license") {
                    Text(license)
                        .foregroundStyle(isLicenseWarning(license) ? .orange : .primary)
                }
            }

            if let installed = dateText(skill.installedAt) {
                LabeledContent("settings.skills.installedAt") {
                    Text(installed)
                }
            }

            if let updated = dateText(skill.updatedAt) {
                LabeledContent("settings.skills.updatedAt") {
                    Text(updated)
                }
            }

            if let size = skill.sizeBytes {
                LabeledContent("settings.skills.size") {
                    Text(byteText(size))
                        .monospacedDigit()
                }
            }

            if let hasScripts = skill.hasScripts {
                LabeledContent("settings.skills.scripts") {
                    Text(
                        hasScripts
                            ? LocalizedStringKey("settings.skills.scripts.yes")
                            : LocalizedStringKey("settings.skills.scripts.no")
                    )
                    .foregroundStyle(hasScripts ? .orange : .secondary)
                }
            }
        }
    }

    private var filesSection: some View {
        Section("settings.skills.files") {
            ForEach(model.files) { file in
                HStack(spacing: 8) {
                    Text(file.path)
                        .font(.caption)
                        .monospaced()
                        .textSelection(.enabled)

                    Spacer(minLength: 8)

                    Text(byteText(file.size))
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    @ViewBuilder
    private func contentSection(_ skill: Skill) -> some View {
        if model.isLoadingContent {
            ProgressView().controlSize(.small)
        } else if let error = model.contentError {
            VStack(alignment: .leading, spacing: 6) {
                Text(error)
                    .foregroundStyle(.red)
                Button("settings.skills.retry") {
                    Task { await model.reloadContent(skill) }
                }
                .buttonStyle(.link)
                .font(.caption)
            }
        } else {
            let preview = withoutFrontmatter(model.content)

            if preview.isEmpty {
                Text("settings.skills.contentEmpty")
                    .foregroundStyle(.secondary)
            } else {
                ScrollView {
                    StructuredText(markdown: preview)
                        .textual.structuredTextStyle(.gitHub)
                        .textual.textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 360)
                .scrollIndicators(.hidden)
            }
        }
    }

    private func row(_ skill: Skill) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 3) {
                Text(skill.name)

                Text(subtitle(skill))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }

            Spacer(minLength: 4)

            if skill.hasScripts == true {
                Image(systemName: "terminal")
                    .foregroundStyle(.secondary)
                    .font(.caption)
                    .help("settings.skills.scripts")
            }
        }
        .padding(.vertical, 2)
    }

    private func subtitle(_ skill: Skill) -> String {
        guard let description = skill.description,
              !description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            return skill.id
        }
        return description
    }

    private var filteredSkills: [Skill] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return model.sortedSkills }

        return model.sortedSkills.filter { skill in
            skill.name.localizedCaseInsensitiveContains(needle)
                || skill.id.localizedCaseInsensitiveContains(needle)
                || (skill.description?.localizedCaseInsensitiveContains(needle) ?? false)
        }
    }

    private var selectionBinding: Binding<String?> {
        Binding(
            get: { model.selected?.id },
            set: { id in
                guard let id,
                      id != model.selected?.id,
                      let skill = model.skills.first(where: { $0.id == id })
                else {
                    return
                }
                Task { await model.select(skill) }
            }
        )
    }

    private func hasMetadata(_ skill: Skill) -> Bool {
        skill.sourceKind != nil
            || skill.sourceURL != nil
            || skill.sourceRef != nil
            || skill.sourceSubdir != nil
            || skill.currentHash != nil
            || skill.license != nil
            || skill.installedAt != nil
            || skill.updatedAt != nil
            || skill.sizeBytes != nil
            || skill.hasScripts != nil
    }

    private func sourceText(_ skill: Skill) -> String? {
        // Local imports record the staging path they were copied from; it is
        // usually a temp directory by the time the skill is listed, so show
        // the source kind instead of leaking that path.
        if skill.sourceKind == "local" {
            return sourceKindText("local")
        }
        if let url = skill.sourceURL, !url.trimmingCharacters(in: .whitespaces).isEmpty {
            return url
        }
        if let kind = skill.sourceKind, !kind.trimmingCharacters(in: .whitespaces).isEmpty {
            return sourceKindText(kind)
        }
        return nil
    }

    private func sourceKindText(_ kind: String) -> String {
        switch kind {
        case "git": String(localized: "settings.skills.source.git")
        case "local": String(localized: "settings.skills.source.local")
        case "archive": String(localized: "settings.skills.source.archive")
        case "url": String(localized: "settings.skills.source.url")
        case "market": String(localized: "settings.skills.source.market")
        default: kind
        }
    }

    private func shortHash(_ hash: String) -> String {
        hash.count > 12 ? "\(hash.prefix(12))…" : hash
    }

    private func isLicenseWarning(_ license: String) -> Bool {
        let value = license.uppercased()
        return value.contains("NOASSERTION") || value.contains("SOURCE-AVAILABLE")
    }

    private func dateText(_ value: String?) -> String? {
        guard let value, !value.isEmpty else { return nil }

        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = formatter.date(from: value)
            ?? ISO8601DateFormatter().date(from: value)

        return date?.formatted(date: .abbreviated, time: .shortened) ?? value
    }

    private func byteText(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(
            fromByteCount: Int64(clamping: bytes),
            countStyle: .file
        )
    }

    private func withoutFrontmatter(_ text: String) -> String {
        let normalized = text
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
        let lines = normalized
            .split(separator: "\n", omittingEmptySubsequences: false)

        guard lines.first?.trimmingCharacters(in: .whitespaces) == "---" else {
            return normalized.trimmingCharacters(in: .whitespacesAndNewlines)
        }

        for index in 1..<lines.count {
            if lines[index].trimmingCharacters(in: .whitespaces) == "---" {
                return lines[(index + 1)...]
                    .joined(separator: "\n")
                    .trimmingCharacters(in: .whitespacesAndNewlines)
            }
        }

        return normalized.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

private struct SkillInstallSheet: View {
    let onInstall: (SkillInstallRequest) async throws -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var mode: Mode = .local
    @State private var path = ""
    @State private var source = ""
    @State private var gitRef = ""
    @State private var subdir = ""
    @State private var isInstalling = false
    @State private var error: String?

    private enum Mode: String, CaseIterable, Identifiable {
        case local
        case git

        var id: String { rawValue }
    }

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("settings.skills.add")
                    .font(.title3.weight(.semibold))
                Text("settings.skills.install.marketHint")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.top, 14)

            Form {
                Section {
                    Picker("", selection: $mode) {
                        Text("settings.skills.install.local").tag(Mode.local)
                        Text("settings.skills.install.git").tag(Mode.git)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                }

                if mode == .local {
                    Section("settings.skills.install.local") {
                        HStack(spacing: 8) {
                            TextField(
                                "",
                                text: $path,
                                prompt: Text("settings.skills.install.path")
                            )
                            .labelsHidden()
                            .textFieldStyle(.roundedBorder)

                            Button("settings.skills.install.choose") {
                                chooseDirectory()
                            }
                            .controlSize(.small)
                        }
                    }
                } else {
                    Section("settings.skills.install.git") {
                        TextField(
                            "",
                            text: $source,
                            prompt: Text("settings.skills.install.sourcePrompt")
                        )
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)

                        TextField(
                            "",
                            text: $gitRef,
                            prompt: Text("settings.skills.install.ref")
                        )
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)

                        TextField(
                            "",
                            text: $subdir,
                            prompt: Text("settings.skills.install.subdir")
                        )
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)
                    }
                }

                if let error {
                    Section {
                        Text(error).foregroundStyle(.red)
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

                Button("settings.skills.install") {
                    Task { await install() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canInstall)
            }
            .padding(14)
        }
        .frame(width: 560, height: 430)
    }

    private var canInstall: Bool {
        guard !isInstalling else { return false }
        switch mode {
        case .local:
            return !path.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .git:
            return !source.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    private func chooseDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false

        if panel.runModal() == .OK, let url = panel.url {
            path = url.path
        }
    }

    private func install() async {
        isInstalling = true
        error = nil

        let trimmedPath = path.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedSource = source.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedRef = gitRef.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedSubdir = subdir.trimmingCharacters(in: .whitespacesAndNewlines)

        let request = SkillInstallRequest(
            path: mode == .local ? trimmedPath : nil,
            source: mode == .git ? trimmedSource : nil,
            gitRef: mode == .git && !trimmedRef.isEmpty ? trimmedRef : nil,
            subdir: mode == .git && !trimmedSubdir.isEmpty ? trimmedSubdir : nil
        )

        do {
            try await onInstall(request)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }

        isInstalling = false
    }
}
