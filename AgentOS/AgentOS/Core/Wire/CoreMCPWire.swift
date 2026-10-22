import Foundation

struct CoreMCPScan: Decodable, Sendable {
    let servers: [CoreMCPServer]
    let warnings: [CoreMCPWarning]

    var domain: MCPScan {
        MCPScan(
            servers: servers.map(\.domain),
            warnings: warnings.map(\.domain)
        )
    }
}

struct CoreMCPServer: Decodable, Sendable {
    let id: String
    let spec: JSONValue
    let apps: [String]

    var domain: MCPServer {
        MCPServer(id: id, spec: spec, apps: apps)
    }
}

struct CoreMCPWarning: Decodable, Sendable {
    let app: String
    let message: String

    var domain: MCPSourceWarning {
        MCPSourceWarning(app: app, message: message)
    }
}

struct CoreMCPUpsertBody: Encodable {
    let serverID: String
    let spec: JSONValue
    let apps: [String]

    enum CodingKeys: String, CodingKey {
        case serverID = "serverId"
        case spec
        case apps
    }
}

struct CoreMCPRemoveBody: Encodable {
    let serverID: String

    enum CodingKeys: String, CodingKey {
        case serverID = "serverId"
    }
}
