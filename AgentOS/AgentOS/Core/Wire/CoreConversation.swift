import Foundation

struct CoreConversationSummary: Decodable, Sendable {
    let id: Int
    let folderID: Int
    let title: String?
    let agentType: String
    let status: String
    let kind: String
    let model: String?
    let messageCount: Int
    let childCount: Int
    let pinnedAt: String?
    let updatedAt: String
    let externalId: String?
    let team: CoreConversationTeam?

    enum CodingKeys: String, CodingKey {
        case id
        case folderID = "folder_id"
        case title
        case agentType = "agent_type"
        case status
        case kind
        case model
        case messageCount = "message_count"
        case childCount = "child_count"
        case pinnedAt = "pinned_at"
        case updatedAt = "updated_at"
        case externalId = "external_id"
        case team
    }

    var domain: Conversation {
        Conversation(
            id: id,
            folderID: folderID,
            title: title ?? "",
            agentType: agentType,
            status: status,
            kind: kind,
            model: model,
            messageCount: messageCount,
            childCount: childCount,
            pinnedAt: pinnedAt,
            updatedAt: updatedAt,
            externalId: externalId,
            teamLeaderRoleID: team?.leaderRoleID
        )
    }
}

struct CoreConversationTeam: Decodable, Sendable {
    let leaderRoleID: String

    enum CodingKeys: String, CodingKey {
        case leaderRoleID = "leader_role_id"
    }
}

struct CoreConversationDetail: Decodable, Sendable {
    let summary: CoreConversationSummary
    let turns: [CoreTurn]

    var domain: ConversationDetail {
        ConversationDetail(conversation: summary.domain, turns: turns.map(\.domain))
    }
}

struct CoreTurn: Decodable, Sendable {
    let id: String
    let role: TurnRole
    let timestamp: String
    let blocks: [CoreBlock]

    enum CodingKeys: String, CodingKey {
        case id
        case role
        case timestamp
        case blocks
    }

    var domain: ConversationTurn {
        ConversationTurn(
            id: id,
            role: role,
            timestamp: timestamp,
            blocks: blocks.map(\.domain)
        )
    }
}

struct CoreBlock: Decodable, Sendable {
    let domain: ConversationBlock


    private enum CodingKeys: String, CodingKey {
        case type
        case text
        case toolName = "tool_name"
        case inputPreview = "input_preview"
        case outputPreview = "output_preview"
        case isError = "is_error"
        case message
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let type = try container.decodeIfPresent(String.self, forKey: .type) ?? ""

        switch type {
        case "text":
            domain = .text(try container.decodeIfPresent(String.self, forKey: .text) ?? "")
        case "thinking":
            domain = .thinking(try container.decodeIfPresent(String.self, forKey: .text) ?? "")
        case "tool_use":
            domain = .toolUse(
                name: try container.decodeIfPresent(String.self, forKey: .toolName) ?? "tool",
                preview: try container.decodeIfPresent(String.self, forKey: .inputPreview)
            )
        case "tool_result":
            domain = .toolResult(
                preview: try container.decodeIfPresent(String.self, forKey: .outputPreview),
                isError: try container.decodeIfPresent(Bool.self, forKey: .isError) ?? false
            )
        case "turn_error":
            domain = .turnError(try container.decodeIfPresent(String.self, forKey: .message) ?? "")
        default:
            domain = .other(type)
        }
    }
}

struct CoreListConversationsBody: Encodable {
    let folderIDs: [Int]
    let includeChildren: Bool

    enum CodingKeys: String, CodingKey {
        case folderIDs = "folderIds"
        case includeChildren
    }
}

struct CoreCreateConversationBody: Encodable {
    let folderID: Int
    let agentType: String
    let title: String?
    let team: ConversationTeamRef?

    enum CodingKeys: String, CodingKey {
        case folderID = "folderId"
        case agentType
        case title
        case team
    }
}

struct CoreConversationIDBody: Encodable {
    let conversationID: Int

    enum CodingKeys: String, CodingKey {
        case conversationID = "conversationId"
    }
}

struct CoreFolderConversationBody: Encodable {
    let conversationID: Int
    let tailTurns: Int

    enum CodingKeys: String, CodingKey {
        case conversationID = "conversationId"
        case tailTurns
    }
}
