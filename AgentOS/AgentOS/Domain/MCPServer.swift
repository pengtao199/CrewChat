import Foundation

struct MCPServer: Identifiable, Sendable, Equatable {
    let id: String
    let spec: MCPServerSpec
    let enabled: Bool
    let synced: Bool
    let hash: String
    let createdAt: String
    let updatedAt: String
    let health: MCPServerHealth?

    var name: String { id }

    var transport: String { spec.transport }

    var summary: String {
        switch spec.transport {
        case "stdio":
            let line = ([spec.command ?? ""] + spec.args)
                .filter { !$0.isEmpty }
                .joined(separator: " ")
            return line.isEmpty ? "stdio" : line
        default:
            return spec.url ?? spec.transport
        }
    }

    func availability(enabled: Bool? = nil) -> MCPAvailability {
        MCPAvailability.resolve(enabled: enabled ?? self.enabled, health: health)
    }
}

struct MCPServerSpec: Sendable, Equatable {
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
}

struct MCPServerHealth: Sendable, Equatable {
    let checking: Bool
    let available: Bool
    let checkedAt: String?
    let error: String?
    let toolCount: Int?
}

enum MCPAvailability: Sendable, Equatable {
    case disabled
    case checking
    case available
    case unavailable
    case unknown

    static func resolve(enabled: Bool, health: MCPServerHealth?) -> MCPAvailability {
        guard enabled else { return .disabled }
        guard let health else { return .unknown }
        if health.checking { return .checking }
        if health.checkedAt == nil { return .unknown }
        return health.available ? .available : .unavailable
    }
}

struct MCPSaveResult: Sendable, Equatable {
    let server: MCPServer
    let warnings: [String]
}

struct MCPDeleteResult: Sendable, Equatable {
    let removed: Bool
    let warnings: [String]
}
