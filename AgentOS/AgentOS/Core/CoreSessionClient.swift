import Foundation

final class CoreSessionClient: CoreSession, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func connect(agentType: String, workingDir: String?, roleID: String?, conversationID: String?, sessionID: String?) async throws -> String {
        let result: CoreConnectResult = try await provider.client().send(
            "acp_connect",
            body: CoreConnectBody(
                agentType: agentType,
                workingDir: workingDir,
                roleId: roleID,
                conversationId: conversationID,
                sessionId: sessionID
            )
        )
        return result.connectionID
    }

    func prompt(connectionID: String, text: String, folderID: String, conversationID: String, mentions: [String]) async throws {
        try await provider.client().sendIgnoring(
            "acp_prompt",
            body: CorePromptBody(
                connectionId: connectionID,
                blocks: [.text(text)],
                folderId: folderID,
                conversationId: conversationID,
                mentions: mentions
            )
        )
    }

    func cancel(connectionID: String) async throws {
        try await provider.client().sendIgnoring(
            "acp_cancel",
            body: CoreConnectionIDBody(connectionId: connectionID)
        )
    }

    func events(conversationID: String) -> AsyncStream<CoreEventFrame> {
        guard let endpoint = provider.endpoint() else {
            return AsyncStream { $0.finish() }
        }
        return CoreEventStream(endpoint: endpoint).events(conversationID: conversationID)
    }

    func respondPermission(requestID: String, optionID: String?) async throws {
        try await provider.client().sendIgnoring(
            "acp_respond_permission",
            body: CorePermissionResponseBody(requestId: requestID, optionId: optionID)
        )
    }
}

struct CoreConnectResult: Decodable {
    let connectionID: String

    enum CodingKeys: String, CodingKey {
        case connectionID = "connection_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let value = try container.decode(String.self, forKey: .connectionID)
        guard !value.isEmpty else {
            throw DecodingError.dataCorruptedError(
                forKey: .connectionID,
                in: container,
                debugDescription: "acp_connect returned no connection id"
            )
        }
        connectionID = value
    }
}

struct CoreConnectBody: Encodable {
    let agentType: String
    let workingDir: String?
    let roleId: String?
    let conversationId: String?
    let sessionId: String?
}

struct CoreConnectionIDBody: Encodable {
    let connectionId: String
}

struct CorePromptBody: Encodable {
    let connectionId: String
    let blocks: [CorePromptBlock]
    let folderId: String
    let conversationId: String
    let mentions: [String]
}

struct CorePermissionResponseBody: Encodable {
    let requestId: String
    let optionId: String?
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
