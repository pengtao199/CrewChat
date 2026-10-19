import Foundation

struct CoreAcpAgent: Decodable, Sendable {
    let agentType: String
    let name: String
    let description: String
    let available: Bool
    let enabled: Bool
    let installedVersion: String?
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
