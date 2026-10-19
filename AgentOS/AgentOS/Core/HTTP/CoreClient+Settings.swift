import Foundation

extension CoreClient {
    func delegationSettings() async throws -> CoreDelegationSettings {
        try await send("get_delegation_settings", body: EmptyBody())
    }

    func setDelegationSettings(_ settings: CoreDelegationSettings) async throws -> CoreDelegationSettings {
        struct Body: Encodable {
            let settings: CoreDelegationSettings
        }
        return try await send("set_delegation_settings", body: Body(settings: settings))
    }
}
