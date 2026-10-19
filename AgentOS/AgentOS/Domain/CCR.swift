import Foundation

struct CCRHealth: Sendable, Equatable {
    let url: String
    let tokenConfigured: Bool
}

struct CCRProvider: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let type: String
    let baseURL: String
    let enabled: Bool
    let models: [String]
    let credentials: [CCRCredential]
}

struct CCRCredential: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let apiKey: String
    let enabled: Bool

    var masked: String {
        guard !apiKey.isEmpty else { return "—" }
        return "\(apiKey.prefix(6))…"
    }
}

struct CCRAccount: Identifiable, Sendable, Equatable {
    let id: String
    let label: String
    let status: String
    let meters: [CCRMeter]
}

struct CCRMeter: Identifiable, Sendable, Equatable {
    let id: String
    let label: String
    let unit: String
    let remaining: Double?
    let limit: Double?
    let used: Double?
    let window: String?
    let resetAt: String?
}
