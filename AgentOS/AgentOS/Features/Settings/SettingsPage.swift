import SwiftUI

enum SettingsPage: String, CaseIterable, Identifiable, Hashable {
    case general
    case core
    case about

    var id: String { rawValue }

    var titleKey: LocalizedStringKey {
        switch self {
        case .general: "settings.general"
        case .core: "settings.core"
        case .about: "settings.about"
        }
    }

    var symbol: String {
        switch self {
        case .general: "gearshape"
        case .core: "server.rack"
        case .about: "info.circle"
        }
    }
}
