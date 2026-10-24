import Foundation

protocol CoreWorkspaces: Sendable {
    func workspaces() async throws -> [Workspace]
    func addWorkspace(path: String) async throws -> Workspace
    func removeWorkspace(id: String) async throws
}

protocol CoreConversations: Sendable {
    func conversations(in folderIDs: [String]) async throws -> [Conversation]
    func detail(conversationID: String, tailTurns: Int) async throws -> ConversationDetail
    func createConversation(folderID: String, agentType: String, title: String?, team: ConversationTeamRef?) async throws -> String
    func deleteConversation(id: String) async throws
}

struct ConversationTeamRef: Sendable, Encodable {
    let leaderRoleID: String
    let memberRoleIDs: [String]

    enum CodingKeys: String, CodingKey {
        case leaderRoleID = "leader_role_id"
        case memberRoleIDs = "member_role_ids"
    }
}
