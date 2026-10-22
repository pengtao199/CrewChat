import Foundation

struct CoreAcpAgent: Decodable, Sendable {
    let agentType: String
    let name: String
    let description: String
    let available: Bool
    let enabled: Bool
    let installedVersion: String?
    let registryVersion: String?
    let supportsCustomVersion: Bool
    let distributionType: String
    let isAdapter: Bool
    let env: [String: String]

    enum CodingKeys: String, CodingKey {
        case agentType = "agent_type"
        case name
        case description
        case available
        case enabled
        case installedVersion = "installed_version"
        case registryVersion = "registry_version"
        case supportsCustomVersion = "supports_custom_version"
        case distributionType = "distribution_type"
        case isAdapter = "is_acp_adapter"
        case env
    }

    var domain: Engine {
        Engine(
            agentType: agentType,
            name: name,
            summary: description,
            available: available,
            enabled: enabled,
            installedVersion: installedVersion,
            registryVersion: registryVersion,
            supportsCustomVersion: supportsCustomVersion,
            distributionType: distributionType,
            isAdapter: isAdapter,
            env: env
        )
    }
}

struct CoreAgentPreferences: Encodable {
    let agentType: String
    let enabled: Bool
    let env: [String: String]
}

struct CoreAgentLatestRelease: Decodable, Sendable {
    let version: String
}

struct CorePrepareAgentBody: Encodable {
    let agentType: String
    let registryVersion: String?
    let version: String?
    let cleanFirst: Bool
    let taskId: String
}

struct CoreDownloadAgentBody: Encodable {
    let agentType: String
    let version: String?
    let taskId: String
}

struct CoreAgentTaskBody: Encodable {
    let taskId: String
}

struct CoreDiagnosticsParams: Encodable {
    let agentType: String
}

struct CoreDiagnosticsReport: Decodable, Sendable {
    let verdict: CoreDiagnosticsVerdict
    let sections: [CoreDiagnosticsSection]
    let plainText: String

    enum CodingKeys: String, CodingKey {
        case verdict
        case sections
        case plainText = "plain_text"
    }

    var domain: EngineDiagnostics {
        EngineDiagnostics(
            level: verdict.level,
            summary: verdict.summary,
            sections: sections.map(\.domain),
            plainText: plainText
        )
    }
}

struct CoreDiagnosticsVerdict: Decodable, Sendable {
    let level: String
    let code: String
    let summary: String
}

struct CoreDiagnosticsSection: Decodable, Sendable {
    let title: String
    let checks: [CoreDiagnosticsCheck]

    var domain: EngineDiagnosticsSection {
        EngineDiagnosticsSection(title: title, checks: checks.map(\.domain))
    }
}

struct CoreDiagnosticsCheck: Decodable, Sendable {
    let label: String
    let value: String
    let status: String
    let hint: String?

    var domain: EngineDiagnosticsCheck {
        EngineDiagnosticsCheck(
            label: label,
            value: value,
            status: status,
            hint: hint
        )
    }
}
