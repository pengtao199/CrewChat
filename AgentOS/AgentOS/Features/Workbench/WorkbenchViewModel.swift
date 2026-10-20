import SwiftUI

struct LiveTool: Identifiable, Equatable {
    let id: String
    var title: String
    var status: String
    var output: String?
}

struct LiveTurn: Equatable {
    var text: String
    var thinking: String
    var tools: [LiveTool]
    var isRunning: Bool
}

@MainActor
@Observable
final class WorkbenchViewModel {
    private(set) var workspaces: [Workspace] = []
    private(set) var conversations: [Conversation] = []
    private(set) var detail: ConversationDetail?
    private(set) var engines: [Engine] = []
    private(set) var roles: [AgentRole] = []
    private(set) var isLoading = false
    private(set) var isLoadingDetail = false
    private(set) var errorMessage: String?
    private(set) var isSending = false
    private(set) var isRunning = false
    private(set) var liveConversationID: Int?

    var draftFolder: Workspace?
    var selectedRoleIDs: [String] = []
    var selectedConversationID: Int?

    private var liveText = ""
    private var liveThinking = ""
    private var liveTools: [LiveTool] = []
    private var connectionID: String?
    private var eventTask: Task<Void, Never>?

    private let workspacesClient: CoreWorkspaces
    private let conversationsClient: CoreConversations
    private let enginesClient: CoreEngines
    private let sessionClient: CoreSession
    private let rosterClient: CoreRoster

    init(
        workspaces: CoreWorkspaces,
        conversations: CoreConversations,
        engines: CoreEngines,
        session: CoreSession,
        roster: CoreRoster
    ) {
        self.workspacesClient = workspaces
        self.conversationsClient = conversations
        self.enginesClient = engines
        self.sessionClient = session
        self.rosterClient = roster
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

    func loadEngines() async {
        engines = (try? await enginesClient.engines()) ?? []
    }

    func loadRoles() async {
        roles = (try? await rosterClient.agents()) ?? []
    }

    func startDraft(in workspace: Workspace) {
        draftFolder = workspace
        selectedRoleIDs = []
        Task { await loadRoles() }
    }

    func cancelDraft() {
        draftFolder = nil
        selectedRoleIDs = []
    }

    func toggleRole(_ id: String) {
        if let index = selectedRoleIDs.firstIndex(of: id) {
            selectedRoleIDs.remove(at: index)
        } else {
            selectedRoleIDs.append(id)
        }
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
            if selectedConversationID == detail?.conversation.id {
                selectedConversationID = nil
            }
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func createConversation(in folderID: Int, agentType: String) async -> Int? {
        do {
            let title = String(localized: "conversation.new")
            let id = try await conversationsClient.createConversation(
                folderID: folderID,
                agentType: agentType,
                title: title,
                team: nil
            )
            await load()
            return id
        } catch {
            errorMessage = error.localizedDescription
            return nil
        }
    }

    func archive(_ conversationID: Int) async {
        do {
            try await conversationsClient.deleteConversation(id: conversationID)
            if detail?.conversation.id == conversationID {
                detail = nil
            }
            if selectedConversationID == conversationID {
                selectedConversationID = nil
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
        selectedConversationID = nil
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

    func send(prompt: String) async -> Bool {
        guard let folder = draftFolder, !prompt.isEmpty, !isSending else { return false }
        guard let leaderID = selectedRoleIDs.first,
              let leader = roles.first(where: { $0.id == leaderID }) else {
            return false
        }

        isSending = true
        errorMessage = nil

        do {
            let members = Array(selectedRoleIDs.dropFirst())
            let team = members.isEmpty
                ? nil
                : ConversationTeamRef(leaderRoleID: leaderID, memberRoleIDs: members)

            let conversationID = try await conversationsClient.createConversation(
                folderID: folder.id,
                agentType: leader.agentType,
                title: nil,
                team: team
            )

            let connection = try await sessionClient.connect(
                agentType: leader.agentType,
                workingDir: folder.path,
                roleID: leader.id,
                sessionID: nil
            )

            try await sessionClient.prompt(
                connectionID: connection,
                text: prompt,
                folderID: folder.id,
                conversationID: conversationID
            )

            connectionID = connection
            liveConversationID = conversationID
            selectedConversationID = conversationID
            isRunning = true
            liveText = ""
            liveThinking = ""
            liveTools = []
            draftFolder = nil
            selectedRoleIDs = []

            await load()
            subscribe(connection: connection)
            isSending = false
            return true
        } catch {
            errorMessage = error.localizedDescription
            isSending = false
            return false
        }
    }

    func sendMessage(prompt: String, conversation: Conversation) async -> Bool {
        let text = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !isSending else { return false }
        guard let folder = workspaces.first(where: { $0.id == conversation.folderID }) else {
            return false
        }

        isSending = true
        errorMessage = nil

        do {
            let connection: String
            if let existing = connectionID, liveConversationID == conversation.id {
                connection = existing
            } else {
                connection = try await sessionClient.connect(
                    agentType: conversation.agentType,
                    workingDir: folder.path,
                    roleID: conversation.teamLeaderRoleID,
                    sessionID: conversation.externalId
                )
                connectionID = connection
            }

            try await sessionClient.prompt(
                connectionID: connection,
                text: text,
                folderID: folder.id,
                conversationID: conversation.id
            )

            liveConversationID = conversation.id
            selectedConversationID = conversation.id
            isRunning = true
            liveText = ""
            liveThinking = ""
            liveTools = []

            await open(conversation.id)
            subscribe(connection: connection)
            isSending = false
            return true
        } catch {
            errorMessage = error.localizedDescription
            isSending = false
            return false
        }
    }

    func engineName(for agentType: String) -> String {
        engines.first(where: { $0.agentType == agentType })?.name ?? agentType
    }

    func cancelRun() async {
        guard let connectionID else { return }
        try? await sessionClient.cancel(connectionID: connectionID)
        isRunning = false
    }

    func liveTurn(for conversationID: Int) -> LiveTurn? {
        guard liveConversationID == conversationID else { return nil }
        guard isRunning || !liveText.isEmpty || !liveThinking.isEmpty || !liveTools.isEmpty else {
            return nil
        }
        return LiveTurn(
            text: liveText,
            thinking: liveThinking,
            tools: liveTools,
            isRunning: isRunning
        )
    }

    private func subscribe(connection: String) {
        eventTask?.cancel()
        eventTask = Task { [weak self] in
            guard let self else { return }
            for await event in self.sessionClient.events(connectionID: connection) {
                self.handle(event)
            }
        }
    }

    private func handle(_ event: CoreEvent) {
        switch event {
        case let .contentDelta(text):
            liveText += text
        case let .thinking(text):
            liveThinking += text
        case let .toolCall(id, title, status):
            if let index = liveTools.firstIndex(where: { $0.id == id }) {
                liveTools[index].title = title
                liveTools[index].status = status
            } else {
                liveTools.append(LiveTool(id: id, title: title, status: status, output: nil))
            }
        case let .toolUpdate(id, title, status, output):
            guard let index = liveTools.firstIndex(where: { $0.id == id }) else { return }
            if let title {
                liveTools[index].title = title
            }
            if let status {
                liveTools[index].status = status
            }
            if let output {
                liveTools[index].output = output
            }
        case .turnComplete:
            isRunning = false
            let id = liveConversationID
            let text = liveText
            let thinking = liveThinking
            let tools = liveTools
            Task {
                if let id {
                    await open(id)
                    await load()
                }
                if self.liveConversationID == id,
                   self.liveText == text,
                   self.liveThinking == thinking,
                   self.liveTools == tools {
                    self.liveText = ""
                    self.liveThinking = ""
                    self.liveTools = []
                }
            }
        case .detached:
            isRunning = false
        case let .status(value):
            if value == "idle" || value == "ready" {
                isRunning = false
            }
        case .other:
            break
        }
    }
}
