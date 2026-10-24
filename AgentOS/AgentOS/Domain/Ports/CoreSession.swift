import Foundation

protocol CoreSession: Sendable {
    func connect(agentType: String, workingDir: String?, roleID: String?, sessionID: String?) async throws -> String
    func prompt(connectionID: String, text: String, folderID: String, conversationID: String) async throws
    func cancel(connectionID: String) async throws
    func events(connectionID: String) -> AsyncStream<CoreEvent>
}
