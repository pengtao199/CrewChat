import Foundation

struct PermissionFace: Sendable, Equatable {
    let engine: String
    let supportsSessionMode: Bool
    let supportsAutoApprove: Bool
    let modes: [PermissionMode]
}

struct PermissionMode: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let description: String
    let source: String
}
