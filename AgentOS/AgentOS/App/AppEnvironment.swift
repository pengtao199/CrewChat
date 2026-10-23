import SwiftUI

@MainActor
@Observable
final class AppEnvironment {
    let supervisor: CoreSupervisor
    let connection: ConnectionViewModel
    let coreSettings: CoreSettingsViewModel
    let models: ModelsSettingsViewModel
    let roster: AgentsSettingsViewModel
    let skills: SkillsSettingsViewModel
    let mcp: MCPSettingsViewModel
    let engines: EnginesSettingsViewModel
    let workbench: WorkbenchViewModel
    var mode: AppMode = .workbench

    init() {
        let supervisor = CoreSupervisor()
        let provider = CoreClientProvider(
            endpoint: { [weak supervisor] in supervisor?.currentEndpoint() }
        )
        let enginesClient = CoreEnginesClient(provider: provider)
        let rosterClient = CoreRosterClient(provider: provider)
        let capabilitiesClient = CoreCapabilitiesClient(provider: provider)
        let skillsClient = CoreSkillsClient(provider: provider)
        let mcpClient = CoreMCPClient(provider: provider)
        let workspacesClient = CoreWorkspacesClient(provider: provider)
        let conversationsClient = CoreConversationsClient(provider: provider)
        let sessionClient = CoreSessionClient(provider: provider)

        self.supervisor = supervisor
        self.connection = ConnectionViewModel(lifecycle: supervisor)
        self.coreSettings = CoreSettingsViewModel(
            settings: CoreSettingsClient(provider: provider),
            lifecycle: supervisor
        )
        self.models = ModelsSettingsViewModel(ccr: CoreCCRClient(provider: provider))
        self.roster = AgentsSettingsViewModel(roster: rosterClient, engines: enginesClient, capabilities: capabilitiesClient)
        self.skills = SkillsSettingsViewModel(skills: skillsClient)
        self.mcp = MCPSettingsViewModel(mcp: mcpClient)
        self.engines = EnginesSettingsViewModel(engines: enginesClient)
        self.workbench = WorkbenchViewModel(
            workspaces: workspacesClient,
            conversations: conversationsClient,
            engines: enginesClient,
            session: sessionClient,
            roster: rosterClient
        )
    }
}
