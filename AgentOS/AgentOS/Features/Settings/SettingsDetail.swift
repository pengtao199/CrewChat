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
        case .engines:
            EnginesSettingsView()
        case .about:
            AboutSettingsView()
        }
    }
}
