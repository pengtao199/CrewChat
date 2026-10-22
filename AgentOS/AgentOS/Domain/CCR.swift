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
    /// Models the user switched off: out of `models` (CCR's routable list) but
    /// kept here so the row stays visible and can be switched back on.
    let disabledModels: [String]
}

struct CCRCredential: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let apiKey: String
    let enabled: Bool
    let priority: Int?
    let weight: Double?
    let limits: JSONValue?
    /// Provider-level `api_key` surfaced as a row: it has no `enabled` flag in
    /// CCR, so the switch on this row stays disabled.
    let isSynthetic: Bool

    /// Short tail the codeg model page shows next to the row (`…abc123`).
    var keyTail: String {
        let trimmed = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "—" }
        guard trimmed.count > 8 else { return trimmed }
        return "…" + trimmed.suffix(6)
    }

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
    let enabled: Bool
    let models: [String]
    let credentials: [CCRCredentialDraft]
}

struct CCRCredentialDraft: Sendable, Equatable {
    var id: String?
    var name: String
    var apiKey: String
    var enabled: Bool
    var priorityText: String
    var weightText: String
    var limitsText: String

    var priority: Int? {
        Int(priorityText.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    var weight: Double? {
        Double(weightText.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// Empty means "remove the stored limits" (encoded as JSON null).
    var limits: JSONValue? {
        let trimmed = limitsText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .null }
        return try? JSONValue.parse(trimmed)
    }

    var limitsIsValid: Bool {
        let trimmed = limitsText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return true }
        guard let value = try? JSONValue.parse(trimmed) else { return false }
        return value.objectValue != nil
    }
}

struct CCRProviderPreset: Identifiable, Sendable, Equatable {
    let id: String
    let name: String
    let endpoints: [CCRPresetEndpoint]
    let defaultModels: [String]
}

struct CCRPresetEndpoint: Sendable, Equatable {
    let baseURL: String
    let protocols: [String]
}

struct CCRProbeResult: Sendable {
    let models: [String]
    let baseURL: String?
    let protocolName: String?
}
