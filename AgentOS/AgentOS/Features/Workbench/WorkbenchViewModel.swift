import SwiftUI

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
    private(set) var liveConversationID: String?
    private(set) var pendingUserText: String?
    var pendingPermissions: [PendingPermission] = []

    var liveReplies: [String: LiveTurn] = [:]
    var pendingLiveRemoval: [String: LiveTurn] = [:]
    var pendingStreamChunks: [String: PendingStreamChunk] = [:]
    var streamFlushTask: Task<Void, Never>?
    var draftFolder: Workspace?
    var selectedRoleIDs: [String] = []
    var selectedConversationID: String?

    var connectionID: String?
    var waitingForTurn = false
    var eventTask: Task<Void, Never>?
    var refreshTask: Task<Void, Never>?
    var subscribedConversationID: String?

    let workspacesClient: CoreWorkspaces
    let conversationsClient: CoreConversations
    let enginesClient: CoreEngines
    let sessionClient: CoreSession
    let rosterClient: CoreRoster
    let teamClient: CoreTeam

    init(
        workspaces: CoreWorkspaces,
        conversations: CoreConversations,
        engines: CoreEngines,
        session: CoreSession,
        roster: CoreRoster,
        team: CoreTeam
    ) {
        self.workspacesClient = workspaces
        self.conversationsClient = conversations
        self.enginesClient = engines
        self.sessionClient = session
        self.rosterClient = roster
        self.teamClient = team
    }

    func conversations(in workspaceID: String) -> [Conversation] {
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
            if roles.isEmpty {
                await loadRoles()
            }
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

    var canStartTeam: Bool {
        selectedRoleIDs.count >= 2 && selectedRoleIDs.count <= 5
    }

    var selectedLeaderID: String? {
        selectedRoleIDs.first { id in
            roles.contains { role in
                role.id == id && (role.id == "agentos-manager" || role.name == "管理员")
            }
        } ?? selectedRoleIDs.first
    }

    func addWorkspace(path: String) async {
        do {
            _ = try await workspacesClient.addWorkspace(path: path)
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func removeWorkspace(_ id: String) async {
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

    func createConversation(in folderID: String, agentType: String) async -> String? {
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

    func archive(_ conversationID: String) async {
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

    func archiveAll(in folderID: String) async {
        for conversation in conversations(in: folderID) {
            try? await conversationsClient.deleteConversation(id: conversation.id)
        }
        detail = nil
        selectedConversationID = nil
        await load()
    }

    func open(_ conversationID: String) async {
        isLoadingDetail = true
        errorMessage = nil

        if liveConversationID != conversationID {
            eventTask?.cancel()
            eventTask = nil
            refreshTask?.cancel()
            refreshTask = nil
            subscribedConversationID = nil
            liveReplies = [:]
            pendingLiveRemoval = [:]
            pendingStreamChunks = [:]
            streamFlushTask?.cancel()
            streamFlushTask = nil
            pendingPermissions = []
            liveConversationID = conversationID
            isRunning = false
        }

        do {
            detail = try await conversationsClient.detail(conversationID: conversationID, tailTurns: 40)
            if let pending = pendingUserText,
               let last = detail?.turns.last(where: { $0.role == .user }),
               last.plainText.trimmingCharacters(in: .whitespacesAndNewlines) == pending {
                pendingUserText = nil
            }
            clearPersistedLive()
            subscribe(conversationID: conversationID)
            refreshRunningState()
        } catch {
            detail = nil
            errorMessage = error.localizedDescription
        }

        isLoadingDetail = false
    }

    func send(prompt: String) async -> Bool {
        guard let folder = draftFolder, !prompt.isEmpty, !isSending else { return false }
        await loadRoles()

        guard canStartTeam, let leaderID = selectedLeaderID,
              let leader = roles.first(where: { $0.id == leaderID }) else {
            errorMessage = String(localized: "conversation.team.minimum")
            return false
        }

        isSending = true
        errorMessage = nil

        do {
            let members = selectedRoleIDs.filter { $0 != leaderID }
            let team = ConversationTeamRef(leaderRoleID: leaderID, memberRoleIDs: members)

            let conversationID = try await conversationsClient.createConversation(
                folderID: folder.id,
                agentType: "",
                title: nil,
                team: team
            )

            liveConversationID = conversationID
            selectedConversationID = conversationID
            pendingUserText = prompt
            isRunning = true
            waitingForTurn = true
            liveReplies = [:]
            pendingLiveRemoval = [:]
            pendingStreamChunks = [:]
            streamFlushTask?.cancel()
            streamFlushTask = nil
            pendingPermissions = []
            draftFolder = nil
            selectedRoleIDs = []

            await load()
            await open(conversationID)

            let connection = try await sessionClient.connect(
                agentType: leader.agentType,
                workingDir: folder.path,
                roleID: leader.id,
                conversationID: conversationID,
                sessionID: nil
            )
            connectionID = connection

            try await sessionClient.prompt(
                connectionID: connection,
                text: prompt,
                folderID: folder.id,
                conversationID: conversationID,
                mentions: []
            )
            isSending = false
            return true
        } catch {
            errorMessage = error.localizedDescription
            isSending = false
            isRunning = false
            waitingForTurn = false
            pendingUserText = nil
            liveReplies = [:]
            pendingLiveRemoval = [:]
            connectionID = nil
            return false
        }
    }

    func sendMessage(prompt: String, conversation: Conversation, mentions: [String]) async -> Bool {
        let text = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !isSending else { return false }
        guard let folder = workspaces.first(where: { $0.id == conversation.folderID }) else {
            return false
        }

        isSending = true
        errorMessage = nil

        let previousConversationID = liveConversationID
        if previousConversationID != conversation.id {
            liveReplies = [:]
            pendingLiveRemoval = [:]
            pendingStreamChunks = [:]
            streamFlushTask?.cancel()
            streamFlushTask = nil
            pendingPermissions = []
            liveConversationID = conversation.id
        }

        selectedConversationID = conversation.id
        pendingUserText = text
        isRunning = true
        waitingForTurn = true
        subscribe(conversationID: conversation.id)

        do {
            let connection: String
            if let existing = connectionID, previousConversationID == conversation.id {
                connection = existing
            } else {
                connection = try await sessionClient.connect(
                    agentType: conversation.agentType,
                    workingDir: folder.path,
                    roleID: conversation.teamLeaderRoleID,
                    conversationID: conversation.id,
                    sessionID: conversation.externalId
                )
                connectionID = connection
            }

            try await sessionClient.prompt(
                connectionID: connection,
                text: text,
                folderID: folder.id,
                conversationID: conversation.id,
                mentions: mentions
            )

            await open(conversation.id)
            isSending = false
            return true
        } catch {
            errorMessage = error.localizedDescription
            isSending = false
            isRunning = false
            waitingForTurn = false
            pendingUserText = nil
            liveReplies = [:]
            pendingLiveRemoval = [:]
            connectionID = nil
            return false
        }
    }

    func engineName(for agentType: String) -> String {
        engines.first(where: { $0.agentType == agentType })?.name ?? agentType
    }

    func agentDisplayName(for conversation: Conversation) -> String {
        if let roleID = conversation.teamLeaderRoleID,
           let role = roles.first(where: { $0.id == roleID }) {
            return role.name
        }
        return engineName(for: conversation.agentType)
    }

    func addMember(conversationID: String, roleID: String) async throws {
        try await teamClient.addMember(conversationID: conversationID, roleID: roleID)
        await loadRoles()
        await open(conversationID)
    }

    func cancelRun() async {
        waitingForTurn = false
        if let connectionID {
            try? await sessionClient.cancel(connectionID: connectionID)
        }
        if let conversationID = detail?.conversation.id {
            try? await teamClient.cancelConversation(id: conversationID)
        }
        isRunning = false
        refreshSoon()
    }

    func respondPermission(_ permission: PendingPermission, optionID: String?) async {
        do {
            try await sessionClient.respondPermission(
                requestID: permission.request.id,
                optionID: optionID
            )
            pendingPermissions.removeAll { $0.id == permission.id }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func liveTurns(for conversationID: String) -> [LiveTurn] {
        guard liveConversationID == conversationID else { return [] }
        return liveReplies.values
            .filter { $0.isRunning || !$0.text.isEmpty || !$0.thinking.isEmpty || !$0.tools.isEmpty }
            .sorted { $0.startedAt < $1.startedAt }
    }

    func member(for roleID: String?) -> ConversationMember? {
        detail?.member(id: roleID)
    }

    func refreshRunningState() {
        let liveRunning = liveReplies.values.contains { $0.isRunning }
        let delegating = detail?.activeDelegations.isEmpty == false
        isRunning = waitingForTurn || liveRunning || delegating || !pendingPermissions.isEmpty
    }

    func setRunning(_ value: Bool) {
        isRunning = value
    }
}
