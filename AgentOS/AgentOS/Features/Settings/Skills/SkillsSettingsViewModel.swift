import Foundation

@MainActor
@Observable
final class SkillsSettingsViewModel {
    private(set) var skills: [Skill] = []
    private(set) var selected: Skill?
    private(set) var content = ""
    private(set) var isLoading = false
    private(set) var isLoadingContent = false
    private(set) var errorMessage: String?
    private(set) var contentError: String?

    private let client: CoreSkills

    init(skills: CoreSkills) {
        self.client = skills
    }

    var sortedSkills: [Skill] {
        skills.sorted {
            $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending
        }
    }

    func load() async {
        isLoading = true
        errorMessage = nil

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
            }
        } catch {
            errorMessage = error.localizedDescription
            skills = []
            selected = nil
            content = ""
        }

        isLoading = false
    }

    func select(_ skill: Skill) async {
        await loadContent(skill)
    }

    func reloadContent(_ skill: Skill) async {
        await loadContent(skill)
    }

    private func loadContent(_ skill: Skill) async {
        selected = skill
        isLoadingContent = true
        contentError = nil

        do {
            let result = try await client.content(skillID: skill.id)
            selected = result.skill
            content = result.content
        } catch {
            contentError = error.localizedDescription
            content = ""
        }

        isLoadingContent = false
    }
}
