import Foundation

final class CoreCapabilitiesClient: CoreCapabilities, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func skills(agentType: String) async throws -> [CapabilityOption] {
        let result: CoreSkillsResult = try await provider.client().send(
            "acp_list_agent_skills",
            body: CoreSkillListBody(agentType: agentType)
        )
        return result.skills.map {
            CapabilityOption(id: $0.id, name: $0.name, detail: $0.description)
        }
    }

    func mcpServers() async throws -> [CapabilityOption] {
        let scan: CoreMcpScan = try await provider.client().send("mcp_scan_local", body: EmptyBody())
        return scan.servers.map { CapabilityOption(id: $0.id, name: $0.id, detail: nil) }
    }
}

struct CoreSkillListBody: Encodable {
    let agentType: String
    let workspacePath: String? = nil
}

struct CoreFaceBody: Encodable {
    let agentType: String
}
