import Foundation

final class CoreCapabilitiesClient: CoreCapabilities, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func skills(agentType: String) async throws -> [CapabilityOption] {
        _ = agentType
        let library: CoreSkillList = try await provider.client().send(
            "skill_list",
            body: EmptyBody()
        )
        return library.skills.map {
            CapabilityOption(id: $0.id, name: $0.name, detail: $0.description)
        }
    }

    func mcpServers() async throws -> [CapabilityOption] {
        let list: CoreMCPList = try await provider.client().send(
            "mcp_list",
            body: EmptyBody()
        )
        return list.servers
            .filter(\.enabled)
            .map {
                CapabilityOption(
                    id: $0.id,
                    name: $0.spec.name.isEmpty ? $0.id : $0.spec.name,
                    detail: $0.spec.transport
                )
            }
    }
}

struct CoreFaceBody: Encodable {
    let agentType: String
}
