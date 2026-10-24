import Foundation

struct CoreConversationSummary: Decodable, Sendable {
    let id: String
    let folderID: String?
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
    let members: [CoreConversationMember]?
    let delegations: [CoreDelegation]?

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
        case members
        case delegations
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        folderID = try container.decodeIfPresent(String.self, forKey: .folderID)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        agentType = try container.decodeIfPresent(String.self, forKey: .agentType) ?? ""
        status = try container.decodeIfPresent(String.self, forKey: .status) ?? "active"
        kind = try container.decodeIfPresent(String.self, forKey: .kind) ?? "single"
        model = try container.decodeIfPresent(String.self, forKey: .model)
        messageCount = try container.decodeIfPresent(Int.self, forKey: .messageCount) ?? 0
        childCount = try container.decodeIfPresent(Int.self, forKey: .childCount) ?? 0
        pinnedAt = try container.decodeIfPresent(String.self, forKey: .pinnedAt)
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
        externalId = try container.decodeIfPresent(String.self, forKey: .externalId)
        team = try container.decodeIfPresent(CoreConversationTeam.self, forKey: .team)
        members = try container.decodeIfPresent([CoreConversationMember].self, forKey: .members)
        delegations = try container.decodeIfPresent([CoreDelegation].self, forKey: .delegations)
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
    let leader: CoreTeamAgentSnapshot?
    let members: [CoreTeamAgentSnapshot]?

    enum CodingKeys: String, CodingKey {
        case leaderRoleID = "leader_role_id"
        case leader
        case members
    }
}

struct CoreTeamAgentSnapshot: Decodable, Sendable {
    let roleID: String
    let version: Int?
    let agent: CoreTeamAgent

    enum CodingKeys: String, CodingKey {
        case roleID = "role_id"
        case version
        case agent
    }
}

struct CoreTeamAgent: Decodable, Sendable {
    let id: String?
    let name: String
    let agentType: String
    let model: String
    let permissionMode: String
    let autoApprove: Bool
    let skills: [String]?
    let mcp: [String]?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case agentType = "agent_type"
        case model
        case permissionMode = "permission_mode"
        case autoApprove = "auto_approve"
        case skills
        case mcp
    }
}

struct CoreConversationMember: Decodable, Sendable {
    let roleID: String
    let name: String
    let version: Int?
    let agentType: String
    let model: String
    let permissionMode: String
    let autoApprove: Bool
    let state: String?
    let connectionID: String?

    enum CodingKeys: String, CodingKey {
        case roleID = "role_id"
        case name
        case version
        case agentType = "agent_type"
        case model
        case permissionMode = "permission_mode"
        case autoApprove = "auto_approve"
        case state
        case connectionID = "connection_id"
    }

    func domain(snapshot: CoreTeamAgentSnapshot?) -> ConversationMember {
        ConversationMember(
            roleID: roleID,
            name: name,
            seat: .member,
            agentType: agentType,
            model: model,
            state: ConversationMemberState(raw: state),
            connectionID: connectionID,
            permissionMode: permissionMode,
            autoApprove: autoApprove,
            skillCount: snapshot?.agent.skills?.count,
            mcpCount: snapshot?.agent.mcp?.count
        )
    }
}

struct CoreDelegation: Decodable, Sendable {
    let id: String
    let conversationID: String
    let leaderRoleID: String
    let memberRoleID: String
    let childConversationID: String?
    let task: String
    let context: String?
    let expectedOutput: String?
    let status: String
    let result: JSONValue?
    let usage: JSONValue?
    let createdAt: String
    let updatedAt: String

    enum CodingKeys: String, CodingKey {
        case id
        case conversationID = "conversation_id"
        case leaderRoleID = "leader_role_id"
        case memberRoleID = "member_role_id"
        case childConversationID = "child_conversation_id"
        case task
        case context
        case expectedOutput = "expected_output"
        case status
        case result
        case usage
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    var domain: ConversationDelegation {
        ConversationDelegation(
            id: id,
            conversationID: conversationID,
            leaderRoleID: leaderRoleID,
            memberRoleID: memberRoleID,
            childConversationID: childConversationID,
            task: task,
            context: context,
            expectedOutput: expectedOutput,
            status: ConversationDelegationStatus(raw: status),
            summary: result?.objectValue?["summary"]?.stringValue
                ?? result?.objectValue?["text"]?.stringValue,
            usage: usage,
            error: result?.objectValue?["error"]?.stringValue,
            createdAt: createdAt,
            updatedAt: updatedAt
        )
    }
}

struct CoreConversationDetail: Decodable, Sendable {
    let summary: CoreConversationSummary
    let turns: [CoreTurn]

    var domain: ConversationDetail {
        ConversationDetail(
            conversation: summary.domain,
            turns: turns.map(\.domain),
            members: members,
            delegations: (summary.delegations ?? []).map(\.domain)
        )
    }

    private var members: [ConversationMember] {
        let snapshots = Dictionary(
            uniqueKeysWithValues: (summary.team?.members ?? []).map { ($0.roleID, $0) }
        )
        var rows: [ConversationMember] = []

        if let leader = summary.team?.leader {
            rows.append(
                ConversationMember(
                    roleID: leader.roleID,
                    name: leader.agent.name,
                    seat: .leader,
                    agentType: leader.agent.agentType,
                    model: leader.agent.model,
                    state: .idle,
                    connectionID: nil,
                    permissionMode: leader.agent.permissionMode,
                    autoApprove: leader.agent.autoApprove,
                    skillCount: leader.agent.skills?.count,
                    mcpCount: leader.agent.mcp?.count
                )
            )
        } else if let leaderID = summary.team?.leaderRoleID {
            rows.append(
                ConversationMember(
                    roleID: leaderID,
                    name: leaderID,
                    seat: .leader,
                    agentType: summary.agentType,
                    model: summary.model ?? "",
                    state: .idle,
                    connectionID: nil,
                    permissionMode: "",
                    autoApprove: false,
                    skillCount: nil,
                    mcpCount: nil
                )
            )
        }

        rows.append(
            contentsOf: (summary.members ?? []).map {
                $0.domain(snapshot: snapshots[$0.roleID])
            }
        )
        return rows
    }
}

struct CoreTurn: Decodable, Sendable {
    let id: String
    let role: TurnRole
    let timestamp: String
    let speakerRoleID: String?
    let blocks: [CoreBlock]
    let usage: TokenUsage?
    let durationMS: Int?
    let model: String?

    enum CodingKeys: String, CodingKey {
        case id
        case role
        case timestamp
        case speakerRoleID = "speaker_role_id"
        case blocks
        case usage
        case durationMS = "duration_ms"
        case model
    }

    var domain: ConversationTurn {
        ConversationTurn(
            id: id,
            role: role,
            speakerRoleID: speakerRoleID,
            timestamp: timestamp,
            blocks: blocks.map(\.domain),
            usage: usage,
            duration: durationMS.map { TimeInterval($0) / 1000 },
            model: model
        )
    }
}

struct CoreBlock: Decodable, Sendable {
    let domain: ConversationBlock

    private enum CodingKeys: String, CodingKey {
        case type
        case text
        case toolCallID = "tool_call_id"
        case toolName = "tool_name"
        case name
        case inputPreview = "input_preview"
        case outputPreview = "output_preview"
        case isError = "is_error"
        case status
        case durationMS = "duration_ms"
        case message
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let type = try container.decodeIfPresent(String.self, forKey: .type) ?? ""
        let duration = try container.decodeIfPresent(Int.self, forKey: .durationMS)
            .map { TimeInterval($0) / 1000 }
        let toolName = try container.decodeIfPresent(String.self, forKey: .toolName)
            ?? container.decodeIfPresent(String.self, forKey: .name)
            ?? "tool"
        let toolID = try container.decodeIfPresent(String.self, forKey: .toolCallID) ?? toolName
        let status = try container.decodeIfPresent(String.self, forKey: .status) ?? ""

        switch type {
        case "text":
            domain = .text(try container.decodeIfPresent(String.self, forKey: .text) ?? "")
        case "thinking":
            domain = .thinking(try container.decodeIfPresent(String.self, forKey: .text) ?? "")
        case "tool_use":
            domain = .toolUse(
                id: toolID,
                name: toolName,
                preview: try container.decodeIfPresent(String.self, forKey: .inputPreview),
                status: status,
                duration: duration
            )
        case "tool_result":
            domain = .toolResult(
                id: toolID,
                name: toolName,
                preview: try container.decodeIfPresent(String.self, forKey: .outputPreview),
                isError: try container.decodeIfPresent(Bool.self, forKey: .isError) ?? false,
                status: status,
                duration: duration
            )
        case "turn_error":
            domain = .turnError(try container.decodeIfPresent(String.self, forKey: .message) ?? "")
        default:
            domain = .other(type)
        }
    }
}

struct CoreListConversationsBody: Encodable {
    let folderIDs: [String]
    let includeChildren: Bool

    enum CodingKeys: String, CodingKey {
        case folderIDs = "folderIds"
        case includeChildren
    }
}

struct CoreCreateConversationBody: Encodable {
    let folderID: String?
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
    let conversationID: String

    enum CodingKeys: String, CodingKey {
        case conversationID = "conversationId"
    }
}

struct CoreFolderConversationBody: Encodable {
    let conversationID: String
    let tailTurns: Int

    enum CodingKeys: String, CodingKey {
        case conversationID = "conversationId"
        case tailTurns
    }
}
