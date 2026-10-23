import SwiftUI

struct SkillsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var query = ""

    private var model: SkillsSettingsViewModel { environment.skills }

    var body: some View {
        MasterDetailLayout {
            sidebar
        } right: {
            detail
        }
        .navigationTitle("settings.skills")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await model.load() }
                } label: {
                    if model.isLoading {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("settings.skills.refresh", systemImage: "arrow.clockwise")
                    }
                }
                .disabled(model.isLoading)
                .help("settings.skills.refresh")
            }
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
                    Section {
                        ForEach(filteredSkills) { skill in
                            row(skill)
                                .tag(skill.id)
                        }
                    } header: {
                        Text(verbatim: "\(String(localized: "settings.skills.all")) (\(filteredSkills.count))")
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

                    if skill.hasUpdate {
                        Label("settings.skills.updateAvailable", systemImage: "arrow.up.circle.fill")
                            .font(.callout)
                            .foregroundStyle(.orange)
                    }
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

            if hasUsage(skill) {
                usageSection(skill)
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

            if let hash = skill.currentHash, !hash.isEmpty {
                LabeledContent("settings.skills.hash") {
                    Text(shortHash(hash))
                        .font(.caption)
                        .monospaced()
                        .textSelection(.enabled)
                        .help(hash)
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

    private func usageSection(_ skill: Skill) -> some View {
        Section("settings.skills.usage") {
            LabeledContent("settings.skills.agents") {
                Text(
                    skill.agentsUsing.isEmpty
                        ? String(localized: "settings.skills.noAgents")
                        : skill.agentsUsing.joined(separator: "、")
                )
                .foregroundStyle(skill.agentsUsing.isEmpty ? .secondary : .primary)
            }

            if !skill.globalLinks.isEmpty {
                LabeledContent("settings.skills.terminalLinks") {
                    HStack(spacing: 10) {
                        ForEach(skill.globalLinks) { link in
                            HStack(spacing: 3) {
                                Text(agentName(link.agentType))
                                Image(systemName: linkSymbol(link.state))
                                    .foregroundStyle(linkColor(link.state))
                            }
                            .font(.caption)
                        }
                    }
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

            if skill.hasUpdate {
                Image(systemName: "arrow.up.circle.fill")
                    .foregroundStyle(.orange)
                    .font(.caption)
                    .help("settings.skills.updateAvailable")
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
            || skill.currentHash != nil
            || skill.license != nil
            || skill.installedAt != nil
            || skill.updatedAt != nil
            || skill.sizeBytes != nil
            || skill.hasScripts != nil
    }

    private func hasUsage(_ skill: Skill) -> Bool {
        !skill.agentsUsing.isEmpty || !skill.globalLinks.isEmpty
    }

    private func sourceText(_ skill: Skill) -> String? {
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

    private func agentName(_ agentType: String) -> String {
        switch agentType {
        case "claude_code": "Claude Code"
        case "codex": "Codex"
        case "open_code": "OpenCode"
        case "gemini": "Gemini"
        default: agentType
        }
    }

    private func linkSymbol(_ state: String) -> String {
        switch state {
        case "linked": "checkmark.circle.fill"
        case "disabled": "minus.circle"
        case "broken": "exclamationmark.triangle.fill"
        case "foreign": "questionmark.circle"
        default: "circle"
        }
    }

    private func linkColor(_ state: String) -> Color {
        switch state {
        case "linked": .green
        case "disabled": .secondary
        case "broken": .orange
        default: .secondary
        }
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
