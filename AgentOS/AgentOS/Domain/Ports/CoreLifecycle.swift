import Foundation

protocol CoreLifecycle: Sendable {
    func phases() -> AsyncStream<CorePhase>
    func start() async
    func stop() async
}
