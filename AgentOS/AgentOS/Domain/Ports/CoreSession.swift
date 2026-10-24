import Foundation

protocol CoreSession: Sendable {
    func connect(agentType: String, workingDir: String?, roleID: String?, conversationID: String?, sessionID: String?) async throws -> String
    func prompt(connectionID: String, text: String, folderID: String, conversationID: String, mentions: [String]) async throws
    func cancel(connectionID: String) async throws
    func events(conversationID: String) -> AsyncStream<CoreEventFrame>
    func respondPermission(requestID: String, optionID: String?) async throws
}
