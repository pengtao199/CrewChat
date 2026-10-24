import Foundation

protocol CoreTeam: Sendable {
    func addMember(conversationID: String, roleID: String) async throws
    func cancelConversation(id: String) async throws
    func cancelDelegation(id: String) async throws
}
