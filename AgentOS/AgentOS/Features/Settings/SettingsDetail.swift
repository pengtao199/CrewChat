import SwiftUI

struct SettingsDetail: View {
    let page: SettingsPage?

    var body: some View {
        switch page ?? .general {
        case .general:
            GeneralSettingsView()
        case .core:
            CoreSettingsView()
        case .models:
            ModelsSettingsView()
        case .agents:
            AgentsSettingsView()
        case .skills:
            SkillsSettingsView()
        case .mcp:
            MCPSettingsView()
        case .engines:
            EnginesSettingsView()
        case .about:
            AboutSettingsView()
        }
    }
}
