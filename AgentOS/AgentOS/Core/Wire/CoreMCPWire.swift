import Foundation

struct CoreMCPList: Decodable, Sendable {
    let servers: [CoreMCPServerRecord]
}

struct CoreMCPServerRecord: Decodable, Sendable {
    let id: String
    let spec: CoreMCPServerSpec
    let enabled: Bool
    let synced: Bool
    let hash: String
    let createdAt: String
    let updatedAt: String
    let health: CoreMCPServerHealth?

    enum CodingKeys: String, CodingKey {
        case id
        case spec
        case enabled
        case synced
        case hash
        case createdAt
        case updatedAt
        case health
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        var spec = try container.decode(CoreMCPServerSpec.self, forKey: .spec)
        if spec.id.isEmpty {
            spec.id = id
        }
        self.spec = spec
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? spec.enabled
        synced = try container.decodeIfPresent(Bool.self, forKey: .synced) ?? false
        hash = try container.decodeIfPresent(String.self, forKey: .hash) ?? ""
        createdAt = try container.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try container.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
        health = try container.decodeIfPresent(CoreMCPServerHealth.self, forKey: .health)
    }

    var domain: MCPServer {
        MCPServer(
            id: id,
            spec: spec.domain,
            enabled: enabled,
            synced: synced,
            hash: hash,
            createdAt: createdAt,
            updatedAt: updatedAt,
            health: health?.domain
        )
    }
}

struct CoreMCPServerHealth: Decodable, Sendable {
    let checking: Bool
    let available: Bool
    let checkedAt: String?
    let error: String?
    let toolCount: Int?

    enum CodingKeys: String, CodingKey {
        case checking
        case available
        case checkedAt
        case error
        case toolCount
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        checking = try container.decodeIfPresent(Bool.self, forKey: .checking) ?? false
        available = try container.decodeIfPresent(Bool.self, forKey: .available) ?? false
        checkedAt = try container.decodeIfPresent(String.self, forKey: .checkedAt)
        error = try container.decodeIfPresent(String.self, forKey: .error)
        toolCount = try container.decodeIfPresent(Int.self, forKey: .toolCount)
    }

    var domain: MCPServerHealth {
        MCPServerHealth(
            checking: checking,
            available: available,
            checkedAt: checkedAt,
            error: error,
            toolCount: toolCount
        )
    }
}

struct CoreMCPServerSpec: Codable, Sendable {
    var id: String
    var name: String
    var transport: String
    var description: String
    var enabled: Bool
    var command: String?
    var args: [String]
    var env: [String: String]
    var url: String?
    var headers: [String: String]
    var bearerToken: String?
    var sessionMode: String?

    init(_ spec: MCPServerSpec) {
        id = spec.id
        name = spec.name
        transport = spec.transport
        description = spec.description
        enabled = spec.enabled
        command = spec.command
        args = spec.args
        env = spec.env
        url = spec.url
        headers = spec.headers
        bearerToken = spec.bearerToken
        sessionMode = spec.sessionMode
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? ""
        name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        transport = try container.decodeIfPresent(String.self, forKey: .transport) ?? "stdio"
        description = try container.decodeIfPresent(String.self, forKey: .description) ?? ""
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled) ?? true
        command = try container.decodeIfPresent(String.self, forKey: .command)
        args = try container.decodeIfPresent([String].self, forKey: .args) ?? []
        env = try container.decodeIfPresent([String: String].self, forKey: .env) ?? [:]
        url = try container.decodeIfPresent(String.self, forKey: .url)
        headers = try container.decodeIfPresent([String: String].self, forKey: .headers) ?? [:]
        bearerToken = try container.decodeIfPresent(String.self, forKey: .bearerToken)
        sessionMode = try container.decodeIfPresent(String.self, forKey: .sessionMode)
    }

    var domain: MCPServerSpec {
        MCPServerSpec(
            id: id,
            name: name,
            transport: transport,
            description: description,
            enabled: enabled,
            command: command,
            args: args,
            env: env,
            url: url,
            headers: headers,
            bearerToken: bearerToken,
            sessionMode: sessionMode
        )
    }
}

struct CoreMCPUpsertResult: Decodable, Sendable {
    let server: CoreMCPServerRecord
    let warnings: [String]

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        server = try container.decode(CoreMCPServerRecord.self, forKey: .server)
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case server
        case warnings
    }

    var domain: MCPSaveResult {
        MCPSaveResult(server: server.domain, warnings: warnings)
    }
}

struct CoreMCPDeleteResult: Decodable, Sendable {
    let removed: Bool
    let warnings: [String]

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        removed = try container.decodeIfPresent(Bool.self, forKey: .removed) ?? false
        warnings = try container.decodeIfPresent([String].self, forKey: .warnings) ?? []
    }

    enum CodingKeys: String, CodingKey {
        case removed
        case warnings
    }

    var domain: MCPDeleteResult {
        MCPDeleteResult(removed: removed, warnings: warnings)
    }
}

struct CoreMCPUpsertBody: Encodable {
    let serverID: String
    let spec: CoreMCPServerSpec

    enum CodingKeys: String, CodingKey {
        case serverID = "serverId"
        case spec
    }
}

struct CoreMCPRemoveBody: Encodable {
    let serverID: String

    enum CodingKeys: String, CodingKey {
        case serverID = "serverId"
    }
}

struct CoreMCPCheckBody: Encodable {
    let serverID: String?

    enum CodingKeys: String, CodingKey {
        case serverID = "serverId"
    }
}
