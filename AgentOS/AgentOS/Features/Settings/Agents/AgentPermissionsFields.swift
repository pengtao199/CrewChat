import SwiftUI

struct AgentPermissionsFields: View {
    @Binding var permissionMode: String
    @Binding var autoApprove: Bool

    let face: PermissionFace?
    let error: String?
    let isLoading: Bool
    let onRetry: () -> Void

    var body: some View {
        if isLoading {
            ProgressView().controlSize(.small)
        } else if let error {
            HStack(spacing: 6) {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                Button("settings.agents.retry", action: onRetry)
                    .buttonStyle(.link)
                    .font(.caption)
            }
        } else {
            Picker("settings.agents.permissionMode", selection: $permissionMode) {
                ForEach(face?.modes ?? []) { mode in
                    Text(mode.name).tag(mode.id)
                }
                if !permissionMode.isEmpty,
                   face?.modes.contains(where: { $0.id == permissionMode }) != true {
                    Text(permissionMode).tag(permissionMode)
                }
            }
        }

        if face?.supportsAutoApprove != false {
            Toggle("settings.agents.autoApprove", isOn: $autoApprove)

            if autoApprove {
                Text("settings.agents.autoApproveHint")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
    }
}
