import Foundation

@MainActor
@Observable
final class SkillsSettingsViewModel {
    private(set) var engines: [Engine] = []
    private(set) var workspaces: [Workspace] = []
    private(set) var catalog = SkillCatalog.empty
    private(set) var selected: SkillItem?
    private(set) var content = ""
    private(set) var isLoading = false
    private(set) var isLoadingContent = false
    private(set) var errorMessage: String?
    private(set) var contentError: String?

    var agentType = ""
    var scope: SkillScope = .global
    var workspaceID: Int?
    var hasUnsavedChanges = false

    private let skills: CoreSkills
    private let enginesClient: CoreEngines
    private let workspacesClient: CoreWorkspaces

    init(skills: CoreSkills, engines: CoreEngines, workspaces: CoreWorkspaces) {
        self.skills = skills
        self.enginesClient = engines
        self.workspacesClient = workspaces
    }

    var workspacePath: String? {
        guard scope == .project, let workspaceID else { return nil }
        return workspaces.first { $0.id == workspaceID }?.path
    }

    var needsWorkspace: Bool {
        scope == .project && workspacePath == nil
    }

    var visibleSkills: [SkillItem] {
        catalog.skills
            .filter { $0.scope == scope }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    var location: SkillLocation? {
        catalog.locations.first { $0.scope == scope }
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        async let engineRows = enginesClient.engines()
        async let workspaceRows = workspacesClient.workspaces()

        do {
            engines = try await engineRows
        } catch {
            errorMessage = error.localizedDescription
        }

        workspaces = (try? await workspaceRows) ?? []

        if agentType.isEmpty {
            agentType = engines.first(where: { $0.available && $0.enabled })?.agentType
                ?? engines.first?.agentType
                ?? ""
        }
        if scope == .project, workspaceID == nil {
            workspaceID = workspaces.first?.id
        }

        await loadCatalog()
        isLoading = false
    }

    func loadCatalog() async {
        guard !agentType.isEmpty else {
            catalog = .empty
            selected = nil
            content = ""
            return
        }

        guard !needsWorkspace else {
            catalog = .empty
            selected = nil
            content = ""
            return
        }

        isLoading = true
        errorMessage = nil

        do {
            catalog = try await skills.catalog(
                agentType: agentType,
                workspacePath: workspacePath
            )

            if let selected,
               let match = visibleSkills.first(where: {
                   $0.id == selected.id && $0.scope == selected.scope
               }) {
                await loadContent(match)
            } else if let first = visibleSkills.first {
                await loadContent(first)
            } else {
                selected = nil
                content = ""
            }
        } catch {
            errorMessage = error.localizedDescription
            catalog = .empty
            selected = nil
            content = ""
        }

        isLoading = false
    }

    func select(_ skill: SkillItem) async {
        await loadContent(skill)
    }

    func saveCurrent(_ text: String) async throws {
        guard let selected else { return }
        try await skills.save(
            agentType: agentType,
            scope: selected.scope,
            skillID: selected.id,
            content: text,
            workspacePath: selected.scope == .project ? workspacePath : nil,
            layout: selected.layout
        )
        await loadCatalog()
    }

    func create(skillID: String, text: String) async throws {
        try await skills.save(
            agentType: agentType,
            scope: scope,
            skillID: skillID,
            content: text,
            workspacePath: scope == .project ? workspacePath : nil,
            layout: .skillDirectory
        )
        await loadCatalog()

        if let created = visibleSkills.first(where: { $0.id == skillID }) {
            await loadContent(created)
        }
    }

    func delete(_ skill: SkillItem) async throws {
        try await skills.delete(
            agentType: agentType,
            scope: skill.scope,
            skillID: skill.id,
            workspacePath: skill.scope == .project ? workspacePath : nil
        )
        await loadCatalog()
    }

    private func loadContent(_ skill: SkillItem) async {
        selected = skill
        isLoadingContent = true
        contentError = nil

        do {
            let result = try await skills.content(
                agentType: agentType,
                scope: skill.scope,
                skillID: skill.id,
                workspacePath: skill.scope == .project ? workspacePath : nil
            )
            selected = result.skill
            content = result.content
        } catch {
            contentError = error.localizedDescription
            content = ""
        }

        isLoadingContent = false
    }
}
