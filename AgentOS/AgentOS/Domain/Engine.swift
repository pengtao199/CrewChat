import Foundation

struct Engine: Identifiable, Sendable, Equatable {
    let agentType: String
    let name: String
    let summary: String
    let available: Bool
    let enabled: Bool
    let installedVersion: String?
    let registryVersion: String?
    let supportsCustomVersion: Bool
    let distributionType: String
    let isAdapter: Bool
    let env: [String: String]

    var id: String { agentType }

    /// CCR/npm dist-tags are not a reliable "installed" signal — an npx agent
    /// is `available` even before its package is fetched. Only a detected
    /// local version means the CLI can actually run.
    var isInstalled: Bool { installedVersion != nil }
}

struct EnginePreferences: Sendable, Equatable {
    let agentType: String
    let enabled: Bool
    let env: [String: String]
}

struct EngineDiagnostics: Sendable, Equatable {
    let level: String
    let summary: String
    let sections: [EngineDiagnosticsSection]
    let plainText: String
}

struct EngineDiagnosticsSection: Identifiable, Sendable, Equatable {
    let title: String
    let checks: [EngineDiagnosticsCheck]

    var id: String { title }
}

struct EngineDiagnosticsCheck: Identifiable, Sendable, Equatable {
    let label: String
    let value: String
    let status: String
    let hint: String?

    var id: String { label }
}
