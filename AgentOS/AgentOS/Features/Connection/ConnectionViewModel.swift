import SwiftUI

@MainActor
@Observable
final class ConnectionViewModel {
    private(set) var phase: CorePhase = .idle
    private(set) var isStarting = false

    private let lifecycle: CoreLifecycle
    private var observation: Task<Void, Never>?

    init(lifecycle: CoreLifecycle) {
        self.lifecycle = lifecycle
        observation = Task { [weak self] in
            guard let self else { return }
            for await phase in lifecycle.phases() {
                self.phase = phase
                self.isStarting = phase.isBusy
            }
        }
    }

    func start() async {
        await lifecycle.start()
    }

    func retry() async {
        await lifecycle.stop()
        await lifecycle.start()
    }

    var statusKey: LocalizedStringKey {
        switch phase {
        case .idle: "connection.status.idle"
        case .starting: "connection.status.starting"
        case .connecting: "connection.status.connecting"
        case .connected: "connection.status.connected"
        case .failed: "connection.status.failed"
        }
    }

    var statusSymbol: String {
        switch phase {
        case .idle: "circle"
        case .starting, .connecting: "clock"
        case .connected: "checkmark.circle.fill"
        case .failed: "exclamationmark.triangle.fill"
        }
    }

    var statusColor: Color {
        switch phase {
        case .idle: .secondary
        case .starting, .connecting: .orange
        case .connected: .green
        case .failed: .red
        }
    }

    var detailKey: LocalizedStringKey {
        switch phase {
        case .idle: "connection.detail.idle"
        case .starting: "connection.detail.starting"
        case .connecting: "connection.detail.connecting"
        case .connected: "connection.detail.connected"
        case .failed: "connection.detail.failed"
        }
    }

    var failureMessage: String? {
        if case let .failed(message) = phase { return message }
        return nil
    }

    var isConnected: Bool {
        if case .connected = phase { return true }
        return false
    }

    var coreVersion: String? {
        if case let .connected(_, version) = phase { return version }
        return nil
    }

    var canStart: Bool {
        if case .idle = phase { return true }
        if case .failed = phase { return true }
        return false
    }
}
