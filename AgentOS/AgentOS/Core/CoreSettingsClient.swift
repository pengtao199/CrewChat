import Foundation

final class CoreSettingsClient: CoreSettings, @unchecked Sendable {
    private let endpointProvider: @Sendable () -> CoreEndpoint?

    init(endpointProvider: @escaping @Sendable () -> CoreEndpoint?) {
        self.endpointProvider = endpointProvider
    }

    func delegation() async throws -> DelegationSettings {
        let wire = try await client().delegationSettings()
        return DelegationSettings(enabled: wire.enabled, depthLimit: wire.depthLimit)
    }

    func setDelegationEnabled(_ enabled: Bool) async throws -> DelegationSettings {
        let client = try client()
        var wire = try await client.delegationSettings()
        wire.enabled = enabled
        let saved = try await client.setDelegationSettings(wire)
        return DelegationSettings(enabled: saved.enabled, depthLimit: saved.depthLimit)
    }

    private func client() throws -> CoreClient {
        guard let endpoint = endpointProvider() else {
            throw CoreError(code: "core_not_ready", message: "核心尚未连接，先启动核心。", detail: nil)
        }
        return CoreClient(endpoint: endpoint)
    }
}
