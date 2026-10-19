import SwiftUI

struct SettingsDetail: View {
    let page: SettingsPage?

    var body: some View {
        switch page ?? .general {
        case .general:
            GeneralSettingsView()
        case .core:
            CoreSettingsView()
        case .about:
            AboutSettingsView()
        }
    }
}
