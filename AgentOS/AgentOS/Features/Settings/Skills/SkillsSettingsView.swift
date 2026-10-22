import SwiftUI

struct SkillsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var query = ""
    @State private var draft = ""
    @State private var savedDraft = ""
    @State private var preview = false
    @State private var isSaving = false
    @State private var actionError: String?
    @State private var pendingAction: PendingAction?
    @State private var showDiscard = false
    @State private var isCreating = false
    @State private var pendingDelete: SkillItem?

    private var model: SkillsSettingsViewModel { environment.skills }

    private enum PendingAction: Equatable {
        case select(SkillItem)
        case engine(String)
        case scope(SkillScope)
        case workspace(Int?)
    }

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
                    isCreating = true
                } label: {
                    Label("settings.skills.new", systemImage: "plus")
                }
                .disabled(model.needsWorkspace || !model.catalog.supported)

                Button {
                    Task { await model.loadCatalog() }
                } label: {
                    Label("settings.skills.refresh", systemImage: "arrow.clockwise")
                }
                .disabled(model.isLoading || model.agentType.isEmpty)
            }
        }
        .onChange(of: model.content) { _, value in
            draft = value
            savedDraft = value
            actionError = nil
        }
        .onChange(of: model.agentType) { _, _ in
            draft = ""
            savedDraft = ""
        }
        .task {
            await model.load()
            draft = model.content
            savedDraft = model.content
        }
        .onDisappear {
            model.hasUnsavedChanges = false
        }
        .onChange(of: isDirty, initial: true) { _, dirty in
            model.hasUnsavedChanges = dirty
        }
        .alert(
            "settings.skills.discard.title",
            isPresented: $showDiscard,
            presenting: pendingAction
        ) { action in
            Button("settings.skills.discard", role: .destructive) {
                perform(action)
            }
            Button("common.cancel", role: .cancel) {
                pendingAction = nil
            }
        } message: { _ in
            Text("settings.skills.discard.message")
        }
        .sheet(isPresented: $isCreating) {
            SkillCreateSheet(
                engineName: engineName,
                scope: model.scope,
                existingIDs: Set(model.visibleSkills.map(\.id))
            ) { skillID, content in
                try await model.create(skillID: skillID, text: content)
            }
        }
        .alert(
            "settings.skills.delete.title",
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: { if !$0 { pendingDelete = nil } }
            ),
            presenting: pendingDelete
        ) { skill in
            Button("common.delete", role: .destructive) {
                Task { await remove(skill) }
            }
            Button("common.cancel", role: .cancel) {
                pendingDelete = nil
            }
        } message: { skill in
            Text(String(format: String(localized: "settings.skills.delete.message"), skill.name))
        }
    }

    private var sidebar: some View {
        VStack(spacing: 0) {
            VStack(spacing: 8) {
                Picker("settings.skills.engine", selection: engineBinding) {
                    ForEach(model.engines) { engine in
                        Text(engine.name).tag(engine.agentType)
                    }
                }
                .labelsHidden()

                Picker("settings.skills.scope", selection: scopeBinding) {
                    Text("settings.skills.scope.global").tag(SkillScope.global)
                    Text("settings.skills.scope.project").tag(SkillScope.project)
                }
                .pickerStyle(.segmented)
                .labelsHidden()

                if model.scope == .project {
                    Picker("settings.skills.workspace", selection: workspaceBinding) {
                        if model.workspaces.isEmpty {
                            Text("settings.skills.noWorkspace").tag(Int?.none)
                        }
                        ForEach(model.workspaces) { workspace in
                            Text(workspace.displayName).tag(Int?.some(workspace.id))
                        }
                    }
                    .labelsHidden()
                    .disabled(model.workspaces.isEmpty)
                }

                TextField(
                    "",
                    text: $query,
                    prompt: Text("settings.skills.search")
                )
                .labelsHidden()
                .textFieldStyle(.roundedBorder)
            }
            .padding(10)

            Divider()

            if model.needsWorkspace {
                Text("settings.skills.needsWorkspace")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                Spacer()
            } else if let message = model.catalog.message, !model.catalog.supported {
                Text(message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
                Spacer()
            } else if model.isLoading, model.visibleSkills.isEmpty {
                ProgressView()
                    .controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if filteredSkills.isEmpty {
                Text("settings.skills.empty.title")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(20)
                Spacer()
            } else {
                List(filteredSkills, selection: selectionBinding) { skill in
                    row(skill)
                        .tag(skill.id)
                }
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
        } else if model.needsWorkspace {
            ContentUnavailableView {
                Label("settings.skills.needsWorkspace.title", systemImage: "folder.badge.questionmark")
            } description: {
                Text("settings.skills.needsWorkspace")
            }
        } else if !model.catalog.supported {
            ContentUnavailableView(
                "settings.skills.unsupported.title",
                systemImage: "books.vertical",
                description: Text(model.catalog.message ?? String(localized: "settings.skills.unsupported"))
            )
        } else if let skill = model.selected {
            editor(skill)
        } else if model.isLoading {
            ProgressView()
                .controlSize(.small)
        } else {
            SelectItemPlaceholder()
        }
    }

    private func editor(_ skill: SkillItem) -> some View {
        Form {
            Section("settings.skills.info") {
                LabeledContent("settings.skills.name", value: skill.name)
                LabeledContent("settings.skills.id", value: skill.id)
                LabeledContent("settings.skills.scope", value: scopeLabel(skill.scope))
                LabeledContent("settings.skills.layout", value: layoutLabel(skill.layout))
                LabeledContent("settings.skills.path") {
                    Text(skill.path)
                        .font(.caption)
                        .textSelection(.enabled)
                }
                if let description = skill.description, !description.isEmpty {
                    Text(description)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                if skill.readOnly {
                    Label("settings.skills.readOnly", systemImage: "lock")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Section {
                HStack {
                    Picker("", selection: $preview) {
                        Text("settings.skills.edit").tag(false)
                        Text("settings.skills.preview").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .frame(width: 160)

                    Spacer()

                    if isDirty {
                        Text("settings.skills.unsaved")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                }

                if model.isLoadingContent {
                    ProgressView().controlSize(.small)
                } else if let contentError = model.contentError {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(contentError)
                            .foregroundStyle(.red)
                        Button("settings.skills.retry") {
                            Task { await model.select(skill) }
                        }
                        .buttonStyle(.link)
                        .font(.caption)
                    }
                } else if preview {
                    ScrollView {
                        StructuredText(markdown: draft)
                            .textual.structuredTextStyle(.gitHub)
                            .textual.textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(minHeight: 280)
                } else {
                    TextEditor(text: $draft)
                        .font(.system(.body, design: .monospaced))
                        .frame(minHeight: 280)
                        .disabled(skill.readOnly)
                }
            } header: {
                Text("settings.skills.content")
            }

            Section {
                if let actionError {
                    Text(actionError)
                        .foregroundStyle(.red)
                }

                HStack {
                    Button("common.delete", role: .destructive) {
                        pendingDelete = skill
                    }
                    .disabled(skill.readOnly || isSaving)

                    Spacer()

                    Button("settings.skills.save") {
                        Task { await save() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(skill.readOnly || !isDirty || isSaving)
                }
            }
        }
        .formStyle(.grouped)
    }

    private var engineName: String {
        model.engines.first { $0.agentType == model.agentType }?.name ?? model.agentType
    }

    private var filteredSkills: [SkillItem] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return model.visibleSkills }
        return model.visibleSkills.filter {
            $0.id.localizedCaseInsensitiveContains(needle)
                || $0.name.localizedCaseInsensitiveContains(needle)
                || ($0.description?.localizedCaseInsensitiveContains(needle) ?? false)
        }
    }

    private func row(_ skill: SkillItem) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 5) {
                Text(skill.name)
                if skill.readOnly {
                    Image(systemName: "lock")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            Text(skill.id)
                .font(.caption2)
                .monospaced()
                .foregroundStyle(.secondary)
            if let description = skill.description, !description.isEmpty {
                Text(description)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
    }

    private var selectionBinding: Binding<String?> {
        Binding(
            get: { model.selected?.id },
            set: { id in
                guard let id,
                      let skill = model.visibleSkills.first(where: { $0.id == id }),
                      skill.id != model.selected?.id
                else {
                    return
                }
                request(.select(skill))
            }
        )
    }

    private var engineBinding: Binding<String> {
        Binding(
            get: { model.agentType },
            set: { value in
                guard value != model.agentType else { return }
                request(.engine(value))
            }
        )
    }

    private var scopeBinding: Binding<SkillScope> {
        Binding(
            get: { model.scope },
            set: { value in
                guard value != model.scope else { return }
                request(.scope(value))
            }
        )
    }

    private var workspaceBinding: Binding<Int?> {
        Binding(
            get: { model.workspaceID },
            set: { value in
                guard value != model.workspaceID else { return }
                request(.workspace(value))
            }
        )
    }

    private var isDirty: Bool {
        draft != savedDraft
    }

    private func request(_ action: PendingAction) {
        if isDirty {
            pendingAction = action
            showDiscard = true
        } else {
            perform(action)
        }
    }

    private func perform(_ action: PendingAction) {
        pendingAction = nil
        showDiscard = false

        switch action {
        case let .select(skill):
            Task { await model.select(skill) }
        case let .engine(value):
            model.agentType = value
            Task { await model.loadCatalog() }
        case let .scope(value):
            model.scope = value
            if value == .project, model.workspaceID == nil {
                model.workspaceID = model.workspaces.first?.id
            }
            Task { await model.loadCatalog() }
        case let .workspace(value):
            model.workspaceID = value
            Task { await model.loadCatalog() }
        }
    }

    private func save() async {
        isSaving = true
        actionError = nil

        do {
            try await model.saveCurrent(draft)
            savedDraft = draft
        } catch {
            actionError = error.localizedDescription
        }

        isSaving = false
    }

    private func remove(_ skill: SkillItem) async {
        pendingDelete = nil
        actionError = nil

        do {
            try await model.delete(skill)
        } catch {
            actionError = error.localizedDescription
        }
    }

    private func scopeLabel(_ scope: SkillScope) -> String {
        switch scope {
        case .global: String(localized: "settings.skills.scope.global")
        case .project: String(localized: "settings.skills.scope.project")
        }
    }

    private func layoutLabel(_ layout: SkillLayout) -> String {
        switch layout {
        case .skillDirectory: String(localized: "settings.skills.layout.directory")
        case .markdownFile: String(localized: "settings.skills.layout.markdown")
        }
    }
}

private struct SkillCreateSheet: View {
    let engineName: String
    let scope: SkillScope
    let existingIDs: Set<String>
    let onCreate: (String, String) async throws -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var skillID = ""
    @State private var content = ""
    @State private var error: String?
    @State private var isSaving = false

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("settings.skills.new")
                    .font(.title3.weight(.semibold))
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 14)

            Form {
                Section("settings.skills.info") {
                    LabeledContent("settings.skills.engine", value: engineName)
                    LabeledContent("settings.skills.scope", value: scope == .global
                        ? String(localized: "settings.skills.scope.global")
                        : String(localized: "settings.skills.scope.project"))
                    TextField("settings.skills.id", text: $skillID)
                        .font(.system(.body, design: .monospaced))

                    if existingIDs.contains(skillID.trimmingCharacters(in: .whitespacesAndNewlines)) {
                        Text("settings.skills.idExists")
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                }

                Section("settings.skills.content") {
                    TextEditor(text: $content)
                        .font(.system(.body, design: .monospaced))
                        .frame(minHeight: 260)

                    Button("settings.skills.template") {
                        content = Self.template(for: skillID)
                    }
                    .buttonStyle(.link)
                    .font(.caption)
                }

                if let error {
                    Section {
                        Text(error)
                            .foregroundStyle(.red)
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

                Button("settings.skills.create") {
                    Task { await create() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!canCreate)
            }
            .padding(14)
        }
        .frame(minWidth: 620, minHeight: 560)
    }

    private var canCreate: Bool {
        !isSaving && isValidID
    }

    private var isValidID: Bool {
        let trimmed = skillID.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !existingIDs.contains(trimmed) else { return false }
        return trimmed.range(of: "^[a-z0-9][a-z0-9._-]*$", options: .regularExpression) != nil
    }

    private func create() async {
        let id = skillID.trimmingCharacters(in: .whitespacesAndNewlines)
        let text = content.isEmpty ? Self.template(for: id) : content

        isSaving = true
        error = nil

        do {
            try await onCreate(id, text)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }

        isSaving = false
    }

    private static func template(for id: String) -> String {
        let name = id.isEmpty ? "my-skill" : id
        return """
        ---
        name: \(name)
        description:
        ---

        # \(name)

        """
    }
}
