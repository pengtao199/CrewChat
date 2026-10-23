import Foundation

enum CoreBinaryLocator {
    static func locateCliLayer() -> URL? {
        if let path = ProcessInfo.processInfo.environment["AGENTOS_CLI_LAYER_BINARY"],
           !path.isEmpty,
           FileManager.default.isExecutableFile(atPath: path) {
            return URL(fileURLWithPath: path)
        }
        let contents = Bundle.main.bundleURL.appending(path: "Contents")
        let candidates = [
            contents.appending(path: "MacOS/crewchat-cli"),
            contents.appending(path: "Resources/crewchat-cli"),
            contents.appending(path: "Frameworks/crewchat-cli"),
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    static func locateSkillLayer() -> URL? {
        if let path = ProcessInfo.processInfo.environment["AGENTOS_SKILL_BINARY"],
           !path.isEmpty,
           FileManager.default.isExecutableFile(atPath: path) {
            return URL(fileURLWithPath: path)
        }
        let contents = Bundle.main.bundleURL.appending(path: "Contents")
        let candidates = [
            contents.appending(path: "MacOS/crewchat-skill"),
            contents.appending(path: "Resources/crewchat-skill"),
            contents.appending(path: "Frameworks/crewchat-skill"),
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    static func locateMCPLayer() -> URL? {
        if let path = ProcessInfo.processInfo.environment["AGENTOS_MCP_BINARY"],
           !path.isEmpty,
           FileManager.default.isExecutableFile(atPath: path) {
            return URL(fileURLWithPath: path)
        }
        let contents = Bundle.main.bundleURL.appending(path: "Contents")
        let candidates = [
            contents.appending(path: "MacOS/crewchat-mcp"),
            contents.appending(path: "Resources/crewchat-mcp"),
            contents.appending(path: "Frameworks/crewchat-mcp"),
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    static func locateModelLayer() -> URL? {
        if let path = ProcessInfo.processInfo.environment["AGENTOS_MODEL_BINARY"],
           !path.isEmpty,
           FileManager.default.isExecutableFile(atPath: path) {
            return URL(fileURLWithPath: path)
        }
        let contents = Bundle.main.bundleURL.appending(path: "Contents")
        let candidates = [
            contents.appending(path: "MacOS/crewchat-model"),
            contents.appending(path: "Resources/crewchat-model"),
            contents.appending(path: "Frameworks/crewchat-model"),
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    static func cliLayerDirectory() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return support.appending(path: "AgentOS/cli", directoryHint: .isDirectory)
    }

    static func modelLayerDirectory() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return support.appending(path: "AgentOS/model", directoryHint: .isDirectory)
    }

    static func logFile() -> URL {
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first!
            .appending(path: "Logs/AgentOS", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        return logs.appending(path: "cli-layer.log")
    }

    static func cliLayerLogFile() -> URL {
        logFile()
    }

    static func skillLayerLogFile() -> URL {
        logFile(named: "skill-layer.log")
    }

    static func mcpLayerLogFile() -> URL {
        logFile(named: "mcp-layer.log")
    }

    static func modelLayerLogFile() -> URL {
        logFile(named: "model-layer.log")
    }

    private static func logFile(named name: String) -> URL {
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first!
            .appending(path: "Logs/AgentOS", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        return logs.appending(path: name)
    }
}
