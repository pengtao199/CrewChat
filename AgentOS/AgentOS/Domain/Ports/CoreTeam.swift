import Foundation

protocol CoreTeam: Sendable {
    func cancelConversation(id: String) async throws
    func cancelDelegation(id: String) async throws
}
