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

    static func modelLayerLogFile() -> URL {
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first!
            .appending(path: "Logs/AgentOS", directoryHint: .isDirectory)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        return logs.appending(path: "model-layer.log")
    }
}
