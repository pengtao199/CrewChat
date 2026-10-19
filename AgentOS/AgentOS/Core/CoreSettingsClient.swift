import Foundation

final class CoreSettingsClient: CoreSettings, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func delegation() async throws -> DelegationSettings {
        let wire = try await provider.client().delegationSettings()
        return DelegationSettings(enabled: wire.enabled, depthLimit: wire.depthLimit)
    }

    func setDelegationEnabled(_ enabled: Bool) async throws -> DelegationSettings {
        let client = try provider.client()
        var wire = try await client.delegationSettings()
        wire.enabled = enabled
        let saved = try await client.setDelegationSettings(wire)
        return DelegationSettings(enabled: saved.enabled, depthLimit: saved.depthLimit)
    }
}
