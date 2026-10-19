import Foundation

protocol CoreSettings: Sendable {
    func delegation() async throws -> DelegationSettings
    func setDelegationEnabled(_ enabled: Bool) async throws -> DelegationSettings
}
