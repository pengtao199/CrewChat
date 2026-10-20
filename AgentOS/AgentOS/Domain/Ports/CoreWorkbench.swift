import Foundation

protocol CoreWorkspaces: Sendable {
    func workspaces() async throws -> [Workspace]
    func addWorkspace(path: String) async throws -> Workspace
    func removeWorkspace(id: Int) async throws
}

protocol CoreConversations: Sendable {
    func conversations(in folderIDs: [Int]) async throws -> [Conversation]
    func detail(conversationID: Int, tailTurns: Int) async throws -> ConversationDetail
    func createConversation(folderID: Int, agentType: String, title: String?) async throws -> Int
    func deleteConversation(id: Int) async throws
}
