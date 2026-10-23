import SwiftUI

struct AgentPermissionsFields: View {
    @Binding var permissionMode: String
    @Binding var autoApprove: Bool

    var body: some View {
        Picker("settings.agents.permissionMode", selection: $permissionMode) {
            Text("settings.agents.permissionMode.default").tag("")
            ForEach(permissionModes, id: \.self) { mode in
                Text(modeLabel(mode)).tag(mode)
            }
            if !permissionMode.isEmpty, !permissionModes.contains(permissionMode) {
                Text(permissionMode).tag(permissionMode)
            }
        }

        Toggle("settings.agents.autoApprove", isOn: $autoApprove)

        if autoApprove {
            Text("settings.agents.autoApproveHint")
                .font(.caption)
                .foregroundStyle(.orange)
        }
    }

    private var permissionModes: [String] {
        ["default", "acceptEdits", "auto", "dontAsk", "bypassPermissions", "plan"]
    }

    private func modeLabel(_ mode: String) -> LocalizedStringKey {
        switch mode {
        case "default": "settings.agents.permissionMode.default"
        case "acceptEdits": "settings.agents.permissionMode.acceptEdits"
        case "auto": "settings.agents.permissionMode.auto"
        case "dontAsk": "settings.agents.permissionMode.dontAsk"
        case "bypassPermissions": "settings.agents.permissionMode.bypass"
        case "plan": "settings.agents.permissionMode.plan"
        default: LocalizedStringKey(mode)
        }
    }
}
