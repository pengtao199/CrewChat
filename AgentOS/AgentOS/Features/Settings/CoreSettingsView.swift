import AppKit
import SwiftUI

struct CoreSettingsView: View {
    @Environment(AppEnvironment.self) private var environment

    var body: some View {
        Form {
            Section {
                LabeledContent("settings.core.binary", value: "cli-layer")
                LabeledContent("settings.core.data") {
                    Text(CoreBinaryLocator.cliLayerDirectory().path)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
                LabeledContent("settings.core.database") {
                    Text(CoreBinaryLocator.cliLayerDirectory().appending(path: "cli-layer.db").path)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
                LabeledContent("settings.core.port", value: String(localized: "settings.core.port.auto"))
            } header: {
                Text("settings.core.section")
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
                        Task { await environment.coreSettings.restartCore() }
                    }
                    .disabled(environment.coreSettings.isRestarting)

                    Button("settings.core.openLog") {
                        NSWorkspace.shared.activateFileViewerSelecting([CoreBinaryLocator.logFile()])
                    }

                    Button("settings.core.openData") {
                        NSWorkspace.shared.open(CoreBinaryLocator.cliLayerDirectory())
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
}
