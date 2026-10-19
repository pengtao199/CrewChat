import Foundation

struct CoreModelFace: Decodable, Sendable {
    let supported: Bool
    let reason: String?
    let models: [CoreFaceModel]
}

struct CoreFaceModel: Decodable, Sendable {
    let provider: String
    let providerID: String
    let model: String

    enum CodingKeys: String, CodingKey {
        case provider
        case providerID = "provider_id"
        case model
    }
}

struct CoreSkillsResult: Decodable, Sendable {
    let supported: Bool
    let message: String?
    let skills: [CoreSkillItem]
}

struct CoreSkillItem: Decodable, Sendable {
    let id: String
    let name: String
    let description: String?
    let readOnly: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case description
        case readOnly = "read_only"
    }
}

struct CoreMcpScan: Decodable, Sendable {
    let servers: [CoreMcpServer]
}

struct CoreMcpServer: Decodable, Sendable {
    let id: String
}
