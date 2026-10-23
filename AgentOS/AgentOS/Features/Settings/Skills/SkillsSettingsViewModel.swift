import Foundation

@MainActor
@Observable
final class SkillsSettingsViewModel {
    private(set) var skills: [Skill] = []
    private(set) var selected: Skill?
    private(set) var content = ""
    private(set) var files: [SkillFile] = []
    private(set) var isLoading = false
    private(set) var isLoadingContent = false
    private(set) var errorMessage: String?
    private(set) var contentError: String?

    private(set) var update: SkillUpdate?
    private(set) var isCheckingUpdate = false
    private(set) var updateCheckError: String?
    private(set) var isUpdating = false
    private(set) var isRemoving = false
    private(set) var actionError: String?

    private let client: CoreSkills

    init(skills: CoreSkills) {
        self.client = skills
    }

    var sortedSkills: [Skill] {
        skills.sorted {
            $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending
        }
    }

    var isBusy: Bool {
        isLoading || isLoadingContent || isCheckingUpdate || isUpdating || isRemoving
    }

    func load() async {
        isLoading = true
        errorMessage = nil
        update = nil
        updateCheckError = nil

        do {
            skills = try await client.library()

            if let selected,
               let match = skills.first(where: { $0.id == selected.id }) {
                self.selected = match
                await loadContent(match)
            } else if let first = sortedSkills.first {
                await loadContent(first)
            } else {
                selected = nil
                content = ""
                files = []
            }
        } catch {
            errorMessage = error.localizedDescription
            skills = []
            selected = nil
            content = ""
            files = []
        }

        isLoading = false
    }

    /// Reload the library, then check the selected skill only. Inline update
    /// checks for all 81 skills would turn refresh into a network storm.
    func refresh() async {
        await load()
        if let selected {
            await checkUpdate(for: selected)
        }
    }

    func select(_ skill: Skill) async {
        update = nil
        updateCheckError = nil
        actionError = nil
        await loadContent(skill)
    }

    func reloadContent(_ skill: Skill) async {
        await loadContent(skill)
    }

    func checkUpdate(for skill: Skill) async {
        guard skill.sourceKind != "local" else {
            update = nil
            updateCheckError = nil
            return
        }
        guard !isCheckingUpdate else { return }

        isCheckingUpdate = true
        update = nil
        updateCheckError = nil
        defer { isCheckingUpdate = false }

        do {
            let result = try await client.checkUpdate(skillID: skill.id)
            update = result.updateAvailable ? result : nil
        } catch {
            updateCheckError = error.localizedDescription
        }
    }

    func applyUpdate(_ skill: Skill) async {
        guard update?.updateAvailable == true, !isUpdating else { return }

        isUpdating = true
        actionError = nil
        defer { isUpdating = false }

        do {
            _ = try await client.update(skillID: skill.id)
            await load()

            if let refreshed = skills.first(where: { $0.id == skill.id }) {
                await select(refreshed)
            }
        } catch {
            actionError = error.localizedDescription
        }
    }

    func remove(_ skill: Skill) async {
        guard !isRemoving else { return }

        isRemoving = true
        actionError = nil
        defer { isRemoving = false }

        do {
            try await client.remove(skillID: skill.id)
            await load()
        } catch {
            actionError = error.localizedDescription
        }
    }

    func install(_ request: SkillInstallRequest) async throws {
        actionError = nil
        let installed = try await client.install(request)
        await load()

        if let match = skills.first(where: { $0.id == installed.id }) {
            await select(match)
        }
    }

    private func loadContent(_ skill: Skill) async {
        selected = skill
        files = []
        isLoadingContent = true
        contentError = nil

        do {
            let result = try await client.content(skillID: skill.id)
            selected = result.skill
            content = result.content
            files = result.files
        } catch {
            contentError = error.localizedDescription
            content = ""
            files = []
        }

        isLoadingContent = false
    }
}
