import Foundation

enum CoreBinaryLocator {
    static func locate() -> URL? {
        if let path = ProcessInfo.processInfo.environment["AGENTOS_CORE_BINARY"],
           !path.isEmpty,
           FileManager.default.isExecutableFile(atPath: path) {
            return URL(fileURLWithPath: path)
        }

        if let saved = Preferences.coreBinaryPath,
           FileManager.default.isExecutableFile(atPath: saved) {
            return URL(fileURLWithPath: saved)
        }

        let contents = Bundle.main.bundleURL.appending(path: "Contents")
        let candidates = [
            contents.appending(path: "MacOS/codeg-server"),
            contents.appending(path: "Resources/codeg-server"),
            contents.appending(path: "Frameworks/codeg-server"),
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

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

    static func dataDirectory() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return support.appending(path: "AgentOS/core", directoryHint: .isDirectory)
    }

    static func cliLayerDirectory() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return support.appending(path: "AgentOS/cli", directoryHint: .isDirectory)
    }

    /// Opt-in: `AGENTOS_CLI_LAYER=1` or a `cli/required` marker. The app keeps
    /// using codeg by default until Phase 3 is verified.
    static func isCliLayerEnabled() -> Bool {
        if ProcessInfo.processInfo.environment["AGENTOS_CLI_LAYER"] == "1" {
            return true
        }
        return FileManager.default.fileExists(
            atPath: cliLayerDirectory().appending(path: "required").path
        )
    }

    static func modelLayerDirectory() -> URL {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        return support.appending(path: "AgentOS/model", directoryHint: .isDirectory)
    }

    static func logFile() -> URL {
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first!
            .appending(path: "Logs/AgentOS", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        return logs.appending(path: "core.log")
    }

    /// A marker the cutover script creates. When present, a failed/missing
    /// model layer is a hard error instead of a silent fallback to CCR.
    static func modelLayerRequiredMarker() -> URL {
        modelLayerDirectory().appending(path: "required")
    }

    static func isModelLayerRequired() -> Bool {
        FileManager.default.fileExists(atPath: modelLayerRequiredMarker().path)
    }

    /// Written by rollback.sh. A disabled model layer means "use CCR", even if
    /// the binary is still in the bundle/scheme.
    static func isModelLayerDisabled() -> Bool {
        FileManager.default.fileExists(
            atPath: modelLayerDirectory().appending(path: "disabled").path
        )
    }

    static func cliLayerLogFile() -> URL {
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first!
            .appending(path: "Logs/AgentOS", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        return logs.appending(path: "cli-layer.log")
    }

    static func modelLayerLogFile() -> URL {
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first!
            .appending(path: "Logs/AgentOS", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        return logs.appending(path: "model-layer.log")
    }
}
