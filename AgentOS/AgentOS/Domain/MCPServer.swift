import Foundation

struct MCPServer: Identifiable, Sendable, Equatable {
    let id: String
    let spec: JSONValue
    let apps: [String]

    var type: String {
        spec.objectValue?["type"]?.stringValue ?? "stdio"
    }

    var summary: String {
        guard let object = spec.objectValue else { return "—" }

        if type == "stdio" {
            let command = object["command"]?.stringValue ?? ""
            let args = (object["args"]?.arrayValue ?? [])
                .compactMap(\.stringValue)
                .joined(separator: " ")
            let line = [command, args]
                .filter { !$0.isEmpty }
                .joined(separator: " ")
            return line.isEmpty ? "—" : line
        }

        return object["url"]?.stringValue ?? type
    }
}

struct MCPSourceWarning: Identifiable, Sendable, Equatable {
    let app: String
    let message: String

    var id: String { "\(app):\(message)" }
}

struct MCPScan: Sendable, Equatable {
    let servers: [MCPServer]
    let warnings: [MCPSourceWarning]

    static let empty = MCPScan(servers: [], warnings: [])
}

enum MCPAppCatalog {
    static let all: [String] = [
        "claude_code",
        "codex",
        "gemini",
        "open_code",
        "open_claw",
        "cline",
        "hermes",
        "code_buddy",
        "kimi_code",
        "grok",
        "cursor",
        "deepseek",
        "qoder",
        "antigravity",
        "pi",
    ]

    static func labelKey(for app: String) -> String {
        switch app {
        case "claude_code": "settings.mcp.app.claudeCode"
        case "codex": "settings.mcp.app.codex"
        case "gemini": "settings.mcp.app.gemini"
        case "open_code": "settings.mcp.app.openCode"
        case "open_claw": "settings.mcp.app.openClaw"
        case "cline": "settings.mcp.app.cline"
        case "hermes": "settings.mcp.app.hermes"
        case "code_buddy": "settings.mcp.app.codeBuddy"
        case "kimi_code": "settings.mcp.app.kimiCode"
        case "grok": "settings.mcp.app.grok"
        case "cursor": "settings.mcp.app.cursor"
        case "deepseek": "settings.mcp.app.deepseek"
        case "qoder": "settings.mcp.app.qoder"
        case "antigravity": "settings.mcp.app.antigravity"
        case "pi": "settings.mcp.app.pi"
        default: app
        }
    }
}
