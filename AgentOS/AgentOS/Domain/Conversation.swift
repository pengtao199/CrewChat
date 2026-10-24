import Foundation

struct Conversation: Identifiable, Sendable, Equatable {
    let id: String
    let folderID: String?
    let title: String
    let agentType: String
    let status: String
    let kind: String
    let model: String?
    let messageCount: Int
    let childCount: Int
    let pinnedAt: String?
    let updatedAt: String
    let externalId: String?
    let teamLeaderRoleID: String?

    var displayTitle: String {
        title.isEmpty ? String(localized: "conversation.untitled") : title
    }

    var updatedLabel: String {
        guard updatedAt.count >= 16 else { return updatedAt }
        let start = updatedAt.index(updatedAt.startIndex, offsetBy: 5)
        let end = updatedAt.index(updatedAt.startIndex, offsetBy: 16)
        return updatedAt[start..<end].replacingOccurrences(of: "T", with: " ")
    }
}
