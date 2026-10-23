import AppKit
import SwiftUI

struct AboutSettingsView: View {
    @Environment(AppEnvironment.self) private var environment

    private var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "—"
    }

    private var buildVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "—"
    }

    var body: some View {
        Form {
            Section {
                LabeledContent("settings.about.app", value: "\(appVersion) (\(buildVersion))")
                LabeledContent("settings.about.core", value: environment.connection.coreVersion ?? "settings.about.core.none")
            }

            Section {
                Button("settings.about.openLog") {
                    NSWorkspace.shared.activateFileViewerSelecting([CoreBinaryLocator.logFile()])
                }
            }
        }
        .formStyle(.grouped)
    }
}
