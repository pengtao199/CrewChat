import Foundation

struct CoreModelHealth: Decodable, Sendable {
    let url: String
    let tokenConfigured: Bool
}

struct CoreModelConfig: Decodable, Sendable {
    let providers: [CoreModelProvider]

    enum CodingKeys: String, CodingKey {
        case providers = "Providers"
    }
}

struct CoreModelProvider: Decodable, Sendable {
    let id: String?
    let name: String
    let type: String?
    let baseURL: String?
    let enabled: Bool?
    let models: [String]?
    let credentials: [CoreModelCredential]?
    let disabledModels: [String]?
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
        case disabledModels
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
        credentials = try container.decodeIfPresent([CoreModelCredential].self, forKey: .credentials)
        disabledModels = try container.decodeIfPresent([String].self, forKey: .disabledModels)
        apiKey = try container.decodeIfPresent(String.self, forKey: .apiKey)
            ?? container.decodeIfPresent(String.self, forKey: .apiKeyAlt)
            ?? container.decodeIfPresent(String.self, forKey: .apikey)
    }
}

struct CoreModelCredential: Decodable, Sendable {
    let id: String?
    let name: String?
    let label: String?
    let apiKey: String?
    let enabled: Bool?
    let priority: Int?
    let weight: Double?
    let limits: JSONValue?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case label
        case apiKey = "api_key"
        case apiKeyAlt = "apiKey"
        case apikey = "apikey"
        case enabled
        case priority
        case weight
        case limits
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
        priority = try container.decodeIfPresent(Int.self, forKey: .priority)
        weight = try container.decodeIfPresent(Double.self, forKey: .weight)
        limits = try container.decodeIfPresent(JSONValue.self, forKey: .limits)
    }
}

struct CoreModelAccount: Decodable, Sendable {
    let credentialId: String?
    let credentialLabel: String?
    let status: String
    let meters: [CoreModelMeter]
}

struct CoreModelMeter: Decodable, Sendable {
    let id: String
    let label: String
    let unit: String?
    let remaining: Double?
    let limit: Double?
    let used: Double?
    let window: String?
    let resetAt: String?
}

enum CoreModelArgument: Encodable {
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

struct CoreModelCall: Encodable {
    let method: String
    let args: [CoreModelArgument]
}

struct CoreModelProviderSave: Encodable {
    let originalId: String?
    let name: String
    let baseURL: String
    let protocolName: String
    let enabled: Bool
    let models: [String]
    let credentials: [CoreModelCredentialSave]

    enum CodingKeys: String, CodingKey {
        case originalId
        case name
        case baseURL = "baseUrl"
        case protocolName = "protocol"
        case enabled
        case models
        case credentials
    }

    init(_ draft: ModelProviderDraft) {
        originalId = draft.originalID
        name = draft.name
        baseURL = draft.baseURL
        protocolName = draft.protocolName
        enabled = draft.enabled
        models = draft.models
        credentials = draft.credentials.map { credential in
            CoreModelCredentialSave(
                id: credential.id?.isEmpty == false ? credential.id : nil,
                name: credential.name,
                apiKey: credential.apiKey,
                priority: credential.priority,
                weight: credential.weight,
                limits: credential.limits ?? .null,
                enabled: credential.enabled
            )
        }
    }
}

struct CoreModelCredentialSave: Encodable {
    let id: String?
    let name: String
    let apiKey: String
    let priority: Int?
    let weight: Double?
    let limits: JSONValue
    let enabled: Bool
}

struct CoreModelIdBody: Encodable {
    let id: String
}

struct CoreModelProbeBody: Encodable {
    let baseURL: String
    let apiKey: String?

    enum CodingKeys: String, CodingKey {
        case baseURL = "baseUrl"
        case apiKey
    }
}

struct CoreModelProbeResult: Decodable, Sendable {
    let models: [String]
    let normalizedBaseUrl: String?
    let detectedProtocol: String?
}

struct CoreModelModelToggle: Encodable {
    let providerId: String
    let model: String
    let enabled: Bool
}

struct CoreModelCredentialToggle: Encodable {
    let providerId: String
    let credentialId: String
    let enabled: Bool
}

struct CoreModelPreset: Decodable, Sendable {
    let id: String
    let name: String
    let endpoints: [CoreModelPresetEndpoint]?
    let defaultModels: [String]?

    var domain: ModelProviderPreset {
        ModelProviderPreset(
            id: id,
            name: name,
            endpoints: (endpoints ?? []).map(\.domain),
            defaultModels: defaultModels ?? []
        )
    }
}

struct CoreModelPresetEndpoint: Decodable, Sendable {
    let baseUrl: String
    let protocols: [String]?

    var domain: ModelPresetEndpoint {
        ModelPresetEndpoint(baseURL: baseUrl, protocols: protocols ?? [])
    }
}
