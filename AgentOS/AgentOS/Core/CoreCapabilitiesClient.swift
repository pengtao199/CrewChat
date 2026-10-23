import Foundation

final class CoreCapabilitiesClient: CoreCapabilities, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func skills(agentType: String) async throws -> [CapabilityOption] {
        _ = agentType
        let catalog: CoreSkillCatalog = try await provider.client().send(
            "skill_list",
            body: EmptyBody()
        )
        return catalog.skills.map {
            CapabilityOption(id: $0.id, name: $0.name, detail: $0.description)
        }
    }

    func mcpServers() async throws -> [CapabilityOption] {
        let scan: CoreMcpScan = try await provider.client().send("mcp_scan_local", body: EmptyBody())
        return scan.servers.map { CapabilityOption(id: $0.id, name: $0.id, detail: nil) }
    }
}

struct CoreFaceBody: Encodable {
    let agentType: String
}
