import SwiftUI

enum SettingsPage: String, CaseIterable, Identifiable, Hashable {
    case general
    case core
    case models
    case agents
    case engines
    case about

    var id: String { rawValue }

    var titleKey: LocalizedStringKey {
        switch self {
        case .general: "settings.general"
        case .core: "settings.core"
        case .models: "settings.models"
        case .agents: "settings.agents"
        case .engines: "settings.engines"
        case .about: "settings.about"
        }
    }

    var symbol: String {
        switch self {
        case .general: "gearshape"
        case .core: "server.rack"
        case .models: "cube.box"
        case .agents: "person.2"
        case .engines: "terminal"
        case .about: "info.circle"
        }
    }
}
