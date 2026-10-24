import Foundation

final class CoreTeamClient: CoreTeam, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func cancelConversation(id: String) async throws {
        try await provider.client().sendIgnoring(
            "cancel_conversation",
            body: CoreConversationIDBody(conversationID: id)
        )
    }

    func cancelDelegation(id: String) async throws {
        try await provider.client().sendIgnoring(
            "cancel_delegation",
            body: CoreDelegationIDBody(delegationId: id)
        )
    }
}

struct CoreDelegationIDBody: Encodable {
    let delegationId: String
}
