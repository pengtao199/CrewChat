import Foundation

struct CoreCCRHealth: Decodable, Sendable {
    let url: String
    let tokenConfigured: Bool
}

struct CoreCCRConfig: Decodable, Sendable {
    let providers: [CoreCCRProvider]

    enum CodingKeys: String, CodingKey {
        case providers = "Providers"
    }
}

struct CoreCCRProvider: Decodable, Sendable {
    let id: String?
    let name: String
    let type: String?
    let baseURL: String?
    let enabled: Bool?
    let models: [String]?
    let credentials: [CoreCCRCredential]?
    let apiKey: String?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case type
        case baseURL = "api_base_url"
        case baseURLAlt = "baseUrl"
        case enabled
        case models
        case credentials
        case apiKey = "api_key"
        case apiKeyAlt = "apiKey"
        case apikey
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id)
        name = try container.decode(String.self, forKey: .name)
        type = try container.decodeIfPresent(String.self, forKey: .type)
        baseURL = try container.decodeIfPresent(String.self, forKey: .baseURL)
            ?? container.decodeIfPresent(String.self, forKey: .baseURLAlt)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled)
        models = try container.decodeIfPresent([String].self, forKey: .models)
        credentials = try container.decodeIfPresent([CoreCCRCredential].self, forKey: .credentials)
        apiKey = try container.decodeIfPresent(String.self, forKey: .apiKey)
            ?? container.decodeIfPresent(String.self, forKey: .apiKeyAlt)
            ?? container.decodeIfPresent(String.self, forKey: .apikey)
    }
}

struct CoreCCRCredential: Decodable, Sendable {
    let id: String?
    let name: String?
    let label: String?
    let apiKey: String?
    let enabled: Bool?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case label
        case apiKey = "api_key"
        case apiKeyAlt = "apiKey"
        case apikey = "apikey"
        case enabled
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id)
        name = try container.decodeIfPresent(String.self, forKey: .name)
        label = try container.decodeIfPresent(String.self, forKey: .label)
        apiKey = try container.decodeIfPresent(String.self, forKey: .apiKey)
            ?? container.decodeIfPresent(String.self, forKey: .apiKeyAlt)
            ?? container.decodeIfPresent(String.self, forKey: .apikey)
        enabled = try container.decodeIfPresent(Bool.self, forKey: .enabled)
    }
}

struct CoreCCRAccount: Decodable, Sendable {
    let credentialId: String?
    let credentialLabel: String?
    let status: String
    let meters: [CoreCCRMeter]
}

struct CoreCCRMeter: Decodable, Sendable {
    let id: String
    let label: String
    let unit: String?
    let remaining: Double?
    let limit: Double?
    let used: Double?
    let window: String?
    let resetAt: String?
}

enum CoreCCRArgument: Encodable {
    case string(String)
    case object([String: Bool])

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case let .string(value):
            try container.encode(value)
        case let .object(value):
            try container.encode(value)
        }
    }
}

struct CoreCCRCall: Encodable {
    let method: String
    let args: [CoreCCRArgument]
}

struct CoreCCRProviderSave: Encodable {
    let originalId: String?
    let name: String
    let baseURL: String
    let protocolName: String
    let apiKey: String?
    let clearApiKey: Bool
    let enabled: Bool
    let models: [String]

    enum CodingKeys: String, CodingKey {
        case originalId
        case name
        case baseURL = "baseUrl"
        case protocolName = "protocol"
        case apiKey
        case clearApiKey
        case enabled
        case models
    }

    init(_ draft: CCRProviderDraft) {
        originalId = draft.originalID
        name = draft.name
        baseURL = draft.baseURL
        protocolName = draft.protocolName
        apiKey = draft.apiKey
        clearApiKey = draft.clearAPIKey
        enabled = draft.enabled
        models = draft.models
    }
}

struct CoreCCRIdBody: Encodable {
    let id: String
}

struct CoreCCRProbeBody: Encodable {
    let baseURL: String
    let apiKey: String?

    enum CodingKeys: String, CodingKey {
        case baseURL = "baseUrl"
        case apiKey
    }
}

struct CoreCCRProbeResult: Decodable, Sendable {
    let models: [String]
    let normalizedBaseUrl: String?
    let detectedProtocol: String?
}
