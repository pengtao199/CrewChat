import Foundation

struct CoreModelProvider: Codable, Sendable {
    let id: Int
    let name: String
    let apiURL: String
    let apiKey: String
    let apiKeyMasked: String
    let agentType: String
    let model: String?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case apiURL = "api_url"
        case apiKey = "api_key"
        case apiKeyMasked = "api_key_masked"
        case agentType = "agent_type"
        case model
    }

    var domain: ModelProvider {
        ModelProvider(
            id: id,
            name: name,
            apiURL: apiURL,
            apiKeyMasked: apiKeyMasked,
            agentType: agentType,
            model: model
        )
    }
}

struct CoreCreateModelProvider: Encodable {
    let name: String
    let apiUrl: String
    let apiKey: String
    let agentType: String
    let model: String?

    init(_ draft: ModelProviderDraft) {
        name = draft.name
        apiUrl = draft.apiURL
        apiKey = draft.apiKey
        agentType = draft.agentType
        model = draft.model.isEmpty ? nil : draft.model
    }
}

struct CoreUpdateModelProvider: Encodable {
    let id: Int
    let name: String
    let apiUrl: String
    let apiKey: String?
    let agentType: String
    let model: String?

    init(id: Int, draft: ModelProviderDraft) {
        self.id = id
        name = draft.name
        apiUrl = draft.apiURL
        apiKey = draft.apiKey.isEmpty ? nil : draft.apiKey
        agentType = draft.agentType
        model = draft.model.isEmpty ? nil : draft.model
    }
}
