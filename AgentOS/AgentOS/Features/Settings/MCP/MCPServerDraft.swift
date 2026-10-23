import Foundation

struct MCPServerDraft: Equatable {
    var id: String
    var name: String
    var transport: String
    var description: String
    var enabled: Bool
    var command: String
    var argsText: String
    var envText: String
    var url: String
    var headersText: String
    var bearerToken: String
    var sessionMode: String

    static let empty = MCPServerDraft(
        id: "",
        name: "",
        transport: "stdio",
        description: "",
        enabled: true,
        command: "",
        argsText: "",
        envText: "",
        url: "",
        headersText: "",
        bearerToken: "",
        sessionMode: ""
    )

    init(
        id: String,
        name: String,
        transport: String,
        description: String,
        enabled: Bool,
        command: String,
        argsText: String,
        envText: String,
        url: String,
        headersText: String,
        bearerToken: String,
        sessionMode: String
    ) {
        self.id = id
        self.name = name
        self.transport = transport
        self.description = description
        self.enabled = enabled
        self.command = command
        self.argsText = argsText
        self.envText = envText
        self.url = url
        self.headersText = headersText
        self.bearerToken = bearerToken
        self.sessionMode = sessionMode
    }

    init(server: MCPServer) {
        let spec = server.spec
        self.init(
            id: server.id,
            name: spec.name,
            transport: spec.transport,
            description: spec.description,
            enabled: spec.enabled,
            command: spec.command ?? "",
            argsText: spec.args.joined(separator: "\n"),
            envText: spec.env
                .sorted { $0.key < $1.key }
                .map { "\($0.key)=\($0.value)" }
                .joined(separator: "\n"),
            url: spec.url ?? "",
            headersText: spec.headers
                .sorted { $0.key < $1.key }
                .map { "\($0.key): \($0.value)" }
                .joined(separator: "\n"),
            bearerToken: spec.bearerToken ?? "",
            sessionMode: spec.sessionMode ?? ""
        )
    }

    var isRemote: Bool { transport != "stdio" }

    var validationMessage: String? {
        if id.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return String(localized: "settings.mcp.idRequired")
        }
        if name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return String(localized: "settings.mcp.nameRequired")
        }
        if isRemote {
            if url.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return String(localized: "settings.mcp.urlRequired")
            }
            if let line = parsedHeaders.invalidLine {
                return String(
                    format: String(localized: "settings.mcp.headers.invalid"),
                    line
                )
            }
        } else {
            if command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return String(localized: "settings.mcp.commandRequired")
            }
            if let line = parsedEnv.invalidLine {
                return String(
                    format: String(localized: "settings.mcp.env.invalid"),
                    line
                )
            }
        }
        return nil
    }

    var isValid: Bool { validationMessage == nil }

    func spec() -> MCPServerSpec? {
        guard validationMessage == nil else { return nil }
        let trimmedID = id.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedDescription = description.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedCommand = command.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedURL = url.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedToken = bearerToken.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedMode = sessionMode.trimmingCharacters(in: .whitespacesAndNewlines)

        return MCPServerSpec(
            id: trimmedID,
            name: trimmedName,
            transport: transport,
            description: trimmedDescription,
            enabled: enabled,
            command: isRemote || trimmedCommand.isEmpty ? nil : trimmedCommand,
            args: isRemote ? [] : parsedArgs,
            env: isRemote ? [:] : parsedEnv.values,
            url: isRemote && !trimmedURL.isEmpty ? trimmedURL : nil,
            headers: isRemote ? parsedHeaders.values : [:],
            bearerToken: isRemote && !trimmedToken.isEmpty ? trimmedToken : nil,
            sessionMode: trimmedMode.isEmpty ? nil : trimmedMode
        )
    }

    var previewSpec: JSONValue {
        var object: [String: JSONValue] = [
            "id": .string(id.trimmingCharacters(in: .whitespacesAndNewlines)),
            "name": .string(name.trimmingCharacters(in: .whitespacesAndNewlines)),
            "transport": .string(transport),
            "description": .string(description),
            "enabled": .bool(enabled),
        ]

        let trimmedCommand = command.trimmingCharacters(in: .whitespacesAndNewlines)
        if !isRemote, !trimmedCommand.isEmpty {
            object["command"] = .string(trimmedCommand)
        }
        if !isRemote {
            object["args"] = .array(parsedArgs.map(JSONValue.string))
            object["env"] = .object(parsedEnv.values.mapValues(JSONValue.string))
        }

        let trimmedURL = url.trimmingCharacters(in: .whitespacesAndNewlines)
        if isRemote, !trimmedURL.isEmpty {
            object["url"] = .string(trimmedURL)
        }
        if isRemote {
            object["headers"] = .object(parsedHeaders.values.mapValues(JSONValue.string))
        }

        let trimmedToken = bearerToken.trimmingCharacters(in: .whitespacesAndNewlines)
        if isRemote, !trimmedToken.isEmpty {
            object["bearerToken"] = .string(trimmedToken)
        }
        let trimmedMode = sessionMode.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedMode.isEmpty {
            object["sessionMode"] = .string(trimmedMode)
        }

        return .object(object)
    }

    private var parsedArgs: [String] {
        argsText
            .split(whereSeparator: { $0.isNewline })
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
    }

    private var parsedEnv: (values: [String: String], invalidLine: Int?) {
        var values: [String: String] = [:]
        var invalidLine: Int?

        for (index, rawLine) in envText.split(whereSeparator: \.isNewline).enumerated() {
            let line = String(rawLine).trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty else { continue }
            guard let separator = line.firstIndex(of: "=") else {
                invalidLine = invalidLine ?? index + 1
                continue
            }
            let key = String(line[..<separator]).trimmingCharacters(in: .whitespaces)
            let value = String(line[line.index(after: separator)...])
                .trimmingCharacters(in: .whitespaces)
            guard !key.isEmpty else {
                invalidLine = invalidLine ?? index + 1
                continue
            }
            values[key] = value
        }

        return (values, invalidLine)
    }

    private var parsedHeaders: (values: [String: String], invalidLine: Int?) {
        var values: [String: String] = [:]
        var invalidLine: Int?

        for (index, rawLine) in headersText.split(whereSeparator: \.isNewline).enumerated() {
            let line = String(rawLine).trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty else { continue }
            guard let separator = line.firstIndex(of: ":") else {
                invalidLine = invalidLine ?? index + 1
                continue
            }
            let key = String(line[..<separator]).trimmingCharacters(in: .whitespaces)
            let value = String(line[line.index(after: separator)...])
                .trimmingCharacters(in: .whitespaces)
            guard !key.isEmpty else {
                invalidLine = invalidLine ?? index + 1
                continue
            }
            values[key] = value
        }

        return (values, invalidLine)
    }
}
