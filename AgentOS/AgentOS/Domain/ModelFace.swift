import Foundation

struct ModelFace: Sendable, Equatable {
    let supported: Bool
    let reason: String?
    let models: [FaceModel]
}

struct FaceModel: Identifiable, Sendable, Equatable {
    let provider: String
    let providerID: String
    let model: String

    var id: String { "\(providerID)|\(model)" }
}

struct CapabilityOption: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let detail: String?
}
