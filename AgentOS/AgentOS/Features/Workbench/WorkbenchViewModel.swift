import SwiftUI

@MainActor
@Observable
final class WorkbenchViewModel {
    private(set) var workspaces: [Workspace] = []
    private(set) var conversations: [Conversation] = []
    private(set) var detail: ConversationDetail?
    private(set) var isLoading = false
    private(set) var isLoadingDetail = false
    private(set) var errorMessage: String?

    private(set) var engines: [Engine] = []

    private let workspacesClient: CoreWorkspaces
    private let conversationsClient: CoreConversations
    private let enginesClient: CoreEngines

    init(workspaces: CoreWorkspaces, conversations: CoreConversations, engines: CoreEngines) {
        self.workspacesClient = workspaces
        self.conversationsClient = conversations
        self.enginesClient = engines
    }

    func loadEngines() async {
        engines = (try? await enginesClient.engines()) ?? []
    }

    func createConversation(in folderID: Int, agentType: String) async -> Int? {
        do {
            let title = String(localized: "conversation.new")
            let id = try await conversationsClient.createConversation(
                folderID: folderID,
                agentType: agentType,
                title: title
            )
            await load()
            return id
        } catch {
            errorMessage = error.localizedDescription
            return nil
        }
    }

    func conversations(in workspaceID: Int) -> [Conversation] {
        conversations
            .filter { $0.folderID == workspaceID }
            .sorted { $0.updatedAt > $1.updatedAt }
    }

    func load() async {
        isLoading = true
        errorMessage = nil

        do {
            workspaces = try await workspacesClient.workspaces()
            conversations = try await conversationsClient.conversations(in: workspaces.map(\.id))
        } catch {
            errorMessage = error.localizedDescription
        }

        isLoading = false
    }

    func addWorkspace(path: String) async {
        do {
            _ = try await workspacesClient.addWorkspace(path: path)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func removeWorkspace(_ id: Int) async {
        do {
            try await workspacesClient.removeWorkspace(id: id)
            if detail?.conversation.folderID == id {
                detail = nil
            }
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func archive(_ conversationID: Int) async {
        do {
            try await conversationsClient.deleteConversation(id: conversationID)
            if detail?.conversation.id == conversationID {
                detail = nil
            }
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func archiveAll(in folderID: Int) async {
        for conversation in conversations(in: folderID) {
            try? await conversationsClient.deleteConversation(id: conversation.id)
        }
        detail = nil
        await load()
    }

    func open(_ conversationID: Int) async {
        isLoadingDetail = true
        errorMessage = nil

        do {
            detail = try await conversationsClient.detail(conversationID: conversationID, tailTurns: 40)
        } catch {
            detail = nil
            errorMessage = error.localizedDescription
        }

        isLoadingDetail = false
    }
}
