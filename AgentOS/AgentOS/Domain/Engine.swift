import Foundation

struct Engine: Identifiable, Sendable, Equatable {
    let agentType: String
    let name: String
    let summary: String
    let available: Bool
    let enabled: Bool
    let installedVersion: String?
    let distributionType: String
    let isAdapter: Bool
    let env: [String: String]

    var id: String { agentType }
}
