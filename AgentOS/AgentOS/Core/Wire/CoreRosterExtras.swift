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

struct CoreMcpScan: Decodable, Sendable {
    let servers: [CoreMcpServer]
}

struct CoreMcpServer: Decodable, Sendable {
    let id: String
}
