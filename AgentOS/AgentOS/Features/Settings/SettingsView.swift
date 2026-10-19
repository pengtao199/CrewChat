import SwiftUI

struct SettingsView: View {
    var body: some View {
        TabView {
            GeneralSettingsView()
                .tabItem { Label("settings.general", systemImage: "gearshape") }

            CoreSettingsView()
                .tabItem { Label("settings.core", systemImage: "server.rack") }

            AboutSettingsView()
                .tabItem { Label("settings.about", systemImage: "info.circle") }
        }
        .frame(width: 560, height: 420)
    }
}
