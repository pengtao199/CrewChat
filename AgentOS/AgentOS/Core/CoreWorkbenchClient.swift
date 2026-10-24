import Foundation

final class CoreWorkspacesClient: CoreWorkspaces, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func workspaces() async throws -> [Workspace] {
        let rows: [CoreFolderDetail] = try await provider.client().send(
            "list_open_folder_details",
            body: EmptyBody()
        )
        return rows.map(\.domain)
    }

    func addWorkspace(path: String) async throws -> Workspace {
        let row: CoreFolderDetail = try await provider.client().send(
            "open_folder",
            body: CorePathBody(path: path)
        )
        return row.domain
    }

    func removeWorkspace(id: String) async throws {
        try await provider.client().sendIgnoring(
            "remove_folder_from_workspace",
            body: CoreFolderIDBody(folderID: id)
        )
    }
}

final class CoreConversationsClient: CoreConversations, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func conversations(in folderIDs: [String]) async throws -> [Conversation] {
        let rows: [CoreConversationSummary] = try await provider.client().send(
            "list_all_conversations",
            body: CoreListConversationsBody(folderIDs: folderIDs, includeChildren: false)
        )
        return rows.map(\.domain)
    }

    func detail(conversationID: String, tailTurns: Int) async throws -> ConversationDetail {
        let detail: CoreConversationDetail = try await provider.client().send(
            "get_folder_conversation",
            body: CoreFolderConversationBody(conversationID: conversationID, tailTurns: tailTurns)
        )
        return detail.domain
    }

    func createConversation(
        folderID: String,
        agentType: String,
        title: String?,
        team: ConversationTeamRef?
    ) async throws -> String {
        try await provider.client().send(
            "create_conversation",
            body: CoreCreateConversationBody(
                folderID: folderID,
                agentType: agentType,
                title: title,
                team: team
            )
        )
    }

    func deleteConversation(id: String) async throws {
        try await provider.client().sendIgnoring(
            "delete_conversation",
            body: CoreConversationIDBody(conversationID: id)
        )
    }
}
