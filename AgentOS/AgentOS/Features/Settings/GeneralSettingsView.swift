import SwiftUI

struct GeneralSettingsView: View {
    @AppStorage("appearance") private var appearance = Appearance.system
    @AppStorage("core.autoStart") private var autoStart = true

    var body: some View {
        Form {
            Section("settings.general.appearance") {
                Picker("settings.general.theme", selection: $appearance) {
                    ForEach(Appearance.allCases) { option in
                        Text(option.titleKey).tag(option)
                    }
                }
                .pickerStyle(.segmented)
            }

            Section {
                Toggle("settings.general.autostart", isOn: $autoStart)
            } header: {
                Text("settings.general.launch")
            } footer: {
                Text("settings.general.autostart.help")
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}
