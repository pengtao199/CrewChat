import Foundation

struct CoreDelegationSettings: Codable, Sendable {
    var enabled: Bool
    var depthLimit: UInt32
    var agentDefaults: [String: CoreAgentDelegationDefaults]?
    var completedCacheMaxMB: UInt32

    enum CodingKeys: String, CodingKey {
        case enabled
        case depthLimit = "depth_limit"
        case agentDefaults = "agent_defaults"
        case completedCacheMaxMB = "completed_cache_max_mb"
    }
}

struct CoreAgentDelegationDefaults: Codable, Sendable {
    var modeID: String?
    var configValues: [String: String]?

    enum CodingKeys: String, CodingKey {
        case modeID = "mode_id"
        case configValues = "config_values"
    }
}
