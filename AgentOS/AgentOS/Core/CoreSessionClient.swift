import Foundation

final class CoreSessionClient: CoreSession, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func connect(agentType: String, workingDir: String?, roleID: String?) async throws -> String {
        try await provider.client().send(
            "acp_connect",
            body: CoreConnectBody(agentType: agentType, workingDir: workingDir, roleId: roleID)
        )
    }

    func prompt(connectionID: String, text: String, folderID: Int, conversationID: Int) async throws {
        try await provider.client().sendIgnoring(
            "acp_prompt",
            body: CorePromptBody(
                connectionId: connectionID,
                blocks: [.text(text)],
                folderId: folderID,
                conversationId: conversationID
            )
        )
    }

    func cancel(connectionID: String) async throws {
        try await provider.client().sendIgnoring(
            "acp_cancel",
            body: CoreConnectionIDBody(connectionId: connectionID)
        )
    }

    func events(connectionID: String) -> AsyncStream<CoreEvent> {
        guard let endpoint = provider.endpoint() else {
            return AsyncStream { $0.finish() }
        }
        return CoreEventStream(endpoint: endpoint).events(connectionID: connectionID)
    }
}

struct CoreConnectBody: Encodable {
    let agentType: String
    let workingDir: String?
    let roleId: String?
}

struct CoreConnectionIDBody: Encodable {
    let connectionId: String
}

struct CorePromptBody: Encodable {
    let connectionId: String
    let blocks: [CorePromptBlock]
    let folderId: Int
    let conversationId: Int
}

enum CorePromptBlock: Encodable {
    case text(String)

    private enum CodingKeys: String, CodingKey {
        case type
        case text
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .text(value):
            try container.encode("text", forKey: .type)
            try container.encode(value, forKey: .text)
        }
    }
}
