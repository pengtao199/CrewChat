import Foundation

struct Workspace: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let alias: String?
    let path: String
    let sortOrder: Int

    var displayName: String {
        if let alias, !alias.isEmpty {
            return "\(alias) [\(name)]"
        }
        return name
    }
}
