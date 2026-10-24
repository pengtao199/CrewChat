import SwiftUI

struct ConversationPane: View {
    @Environment(AppEnvironment.self) private var environment
    let conversationID: String?

    private var model: WorkbenchViewModel { environment.workbench }

    var body: some View {
        Group {
            if let conversationID {
                if let detail = model.detail, detail.conversation.id == conversationID {
                    ConversationView(
                        detail: detail,
                        liveTurns: model.liveTurns(for: detail.conversation.id),
                        pendingPermissions: model.pendingPermissions,
                        pendingUserText: model.pendingUserText,
                        errorMessage: model.errorMessage,
                        agentName: model.agentDisplayName(for: detail.conversation),
                        isRunning: model.isRunning,
                        isSending: model.isSending,
                        engineName: model.engineName(for:),
                        availableRoles: model.roles,
                        onSend: { text, mentions in
                            await model.sendMessage(
                                prompt: text,
                                conversation: detail.conversation,
                                mentions: mentions
                            )
                        },
                        onStop: {
                            Task { await model.cancelRun() }
                        },
                        onAddMember: { roleID in
                            try await model.addMember(
                                conversationID: detail.conversation.id,
                                roleID: roleID
                            )
                        },
                        onPermission: { permission, optionID in
                            Task { await model.respondPermission(permission, optionID: optionID) }
                        }
                    )
                } else if let message = model.errorMessage {
                    ContentUnavailableView(
                        "common.error",
                        systemImage: "exclamationmark.triangle",
                        description: Text(message)
                    )
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else if model.workspaces.isEmpty {
                ContentUnavailableView(
                    "workbench.noWorkspace.title",
                    systemImage: "folder",
                    description: Text("workbench.noWorkspace.description")
                )
            } else {
                ContentUnavailableView(
                    "workbench.noConversation.title",
                    systemImage: "bubble.left.and.bubble.right",
                    description: Text("workbench.noConversation.description")
                )
            }
        }
        .task(id: conversationID) {
            guard let conversationID else { return }
            await model.open(conversationID)
        }
    }
}
