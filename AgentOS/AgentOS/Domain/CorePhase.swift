import Foundation

enum CorePhase: Sendable, Equatable {
    case idle
    case starting
    case connecting
    case connected(CoreEndpoint, version: String)
    case failed(String)

    var isBusy: Bool {
        switch self {
        case .starting, .connecting: true
        default: false
        }
    }

    var endpoint: CoreEndpoint? {
        if case let .connected(endpoint, _) = self { return endpoint }
        return nil
    }
}
