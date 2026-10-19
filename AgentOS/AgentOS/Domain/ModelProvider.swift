import Foundation

struct ModelProvider: Identifiable, Sendable, Equatable {
    let id: Int
    let name: String
    let apiURL: String
    let apiKeyMasked: String
    let agentType: String
    let model: String?
}

struct ModelProviderDraft: Sendable, Equatable {
    var name = ""
    var apiURL = ""
    var apiKey = ""
    var agentType = ""
    var model = ""
}
