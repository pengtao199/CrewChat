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

struct CorePermissionFace: Decodable, Sendable {
    let engine: String
    let supportsSessionMode: Bool
    let supportsAutoApprove: Bool
    let modes: [CorePermissionMode]

    enum CodingKeys: String, CodingKey {
        case engine
        case supportsSessionMode = "supports_session_mode"
        case supportsAutoApprove = "supports_auto_approve"
        case modes
    }

    var domain: PermissionFace {
        PermissionFace(
            engine: engine,
            supportsSessionMode: supportsSessionMode,
            supportsAutoApprove: supportsAutoApprove,
            modes: modes.map(\.domain)
        )
    }
}

struct CorePermissionMode: Decodable, Sendable {
    let id: String
    let name: String
    let description: String
    let source: String

    var domain: PermissionMode {
        PermissionMode(id: id, name: name, description: description, source: source)
    }
}
