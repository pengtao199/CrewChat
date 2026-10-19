import AppKit
import SwiftUI

struct CoreSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var needsRestart = false

    var body: some View {
        Form {
            Section {
                LabeledContent("settings.core.binary") {
                    HStack(spacing: 8) {
                        Text(Preferences.coreBinaryPath ?? "settings.core.binary.auto")
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .foregroundStyle(.secondary)
                        Button("settings.core.choose") { chooseBinary() }
                    }
                }

                LabeledContent("settings.core.data") {
                    HStack(spacing: 8) {
                        Text(CoreBinaryLocator.dataDirectory().path)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .foregroundStyle(.secondary)
                        Button("settings.core.choose") { chooseDataDirectory() }
                    }
                }

                LabeledContent("settings.core.port", value: String(localized: "settings.core.port.auto"))
            } header: {
                Text("settings.core.section")
            } footer: {
                if needsRestart {
                    Text("settings.core.restartHint")
                        .foregroundStyle(.orange)
                }
            }

            Section {
                Toggle(
                    "settings.core.delegation",
                    isOn: Binding(
                        get: { environment.coreSettings.delegationEnabled },
                        set: { value in
                            Task { await environment.coreSettings.setDelegation(value) }
                        }
                    )
                )
                .disabled(!environment.connection.isConnected || !environment.coreSettings.delegationLoaded || environment.coreSettings.isSaving)

                if let message = environment.coreSettings.errorMessage {
                    Text(message)
                        .foregroundStyle(.red)
                } else {
                    Text("settings.core.delegation.help")
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("settings.core.behavior")
            }

            Section("settings.core.actions") {
                HStack(spacing: 12) {
                    Button("settings.core.restart") {
                        Task {
                            await environment.coreSettings.restartCore()
                            needsRestart = false
                        }
                    }
                    .disabled(environment.coreSettings.isRestarting)

                    Button("settings.core.openLog") {
                        NSWorkspace.shared.activateFileViewerSelecting([CoreBinaryLocator.logFile()])
                    }

                    Button("settings.core.openData") {
                        NSWorkspace.shared.open(CoreBinaryLocator.dataDirectory())
                    }

                    Spacer()

                    if environment.coreSettings.isRestarting {
                        ProgressView().controlSize(.small)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .task {
            await environment.coreSettings.load()
        }
    }

    private func chooseBinary() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.treatsFilePackagesAsDirectories = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Preferences.coreBinaryPath = url.path
        needsRestart = true
    }

    private func chooseDataDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Preferences.coreDataDirectory = url.path
        needsRestart = true
    }
}
