import Foundation

protocol CoreSession: Sendable {
    func connect(agentType: String, workingDir: String?, roleID: String?) async throws -> String
    func prompt(connectionID: String, text: String, folderID: Int, conversationID: Int) async throws
    func cancel(connectionID: String) async throws
    func events(connectionID: String) -> AsyncStream<CoreEvent>
}
