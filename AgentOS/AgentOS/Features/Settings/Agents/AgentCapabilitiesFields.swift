import SwiftUI

struct AgentCapabilitiesFields: View {
    @Binding var skills: [String]
    @Binding var mcp: [String]

    let skillOptions: [CapabilityOption]
    let skillError: String?
    let mcpOptions: [CapabilityOption]
    let mcpError: String?
    let isLoading: Bool
    let onRetry: () -> Void

    var body: some View {
        CapabilityMultiSelect(
            title: "settings.agents.skills",
            options: skillOptions,
            knownIDs: Set(skillOptions.map(\.id)),
            selection: $skills,
            isLoading: isLoading,
            error: skillError,
            searchPrompt: "settings.agents.searchSkills",
            isEnabled: true,
            onRetry: onRetry
        )

        CapabilityMultiSelect(
            title: "settings.agents.mcp",
            options: mcpOptions,
            knownIDs: Set(mcpOptions.map(\.id)).union(mcp),
            selection: $mcp,
            isLoading: isLoading,
            error: mcpError,
            searchPrompt: "settings.agents.searchMCP",
            isEnabled: false,
            onRetry: onRetry
        )
    }
}
