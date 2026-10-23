import Foundation

struct MCPServer: Identifiable, Sendable, Equatable {
    let id: String
    let spec: MCPServerSpec
    let enabled: Bool
    let synced: Bool
    let hash: String
    let createdAt: String
    let updatedAt: String

    var name: String {
        let trimmed = spec.name.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? id : trimmed
    }

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

struct MCPSaveResult: Sendable, Equatable {
    let server: MCPServer
    let warnings: [String]
}

struct MCPDeleteResult: Sendable, Equatable {
    let removed: Bool
    let warnings: [String]
}
