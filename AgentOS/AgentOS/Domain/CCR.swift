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
        let trimmed = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "—" }
        guard trimmed.count > 8 else { return "••••••••" }
        return "••••••••" + trimmed.suffix(4)
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

struct CCRProviderDraft: Sendable {
    let originalID: String?
    let name: String
    let baseURL: String
    let protocolName: String
    let apiKey: String?
    let clearAPIKey: Bool
    let enabled: Bool
    let models: [String]
}

struct CCRProbeResult: Sendable {
    let models: [String]
    let baseURL: String?
    let protocolName: String?
}
