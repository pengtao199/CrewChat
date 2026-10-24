import SwiftUI

struct DraftView: View {
    @Environment(AppEnvironment.self) private var environment
    let workspace: Workspace
    @State private var prompt = ""
    @State private var focusRequested = false

    private var model: WorkbenchViewModel { environment.workbench }

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 24)

            VStack(spacing: 20) {
                Image(systemName: "sparkles")
                    .font(.system(size: 34))
                    .foregroundStyle(.secondary)

                Text("draft.title \(workspace.displayName)")
                    .font(.title2)
                    .multilineTextAlignment(.center)

                rolePicker

                if !model.selectedRoleIDs.isEmpty, !model.canStartTeam {
                    Text("draft.team.minimum")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }

                if let message = model.errorMessage {
                    Text(message).foregroundStyle(.red)
                }
            }
            .frame(maxWidth: 720)

            Spacer(minLength: 24)

            Composer(
                text: $prompt,
                focusRequested: $focusRequested,
                placeholder: "draft.placeholder",
                members: [],
                isRunning: false,
                canSend: !prompt.isEmpty && !model.isSending && model.canStartTeam,
                onSend: {
                    let text = prompt
                    Task {
                        if await model.send(prompt: text) {
                            prompt = ""
                        }
                    }
                },
                onStop: {}
            )
            .frame(maxWidth: 760)
        }
        .padding(24)
        .navigationTitle(workspace.displayName)
    }

    private var rolePicker: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("draft.agents")
                .font(.callout)
                .foregroundStyle(.secondary)

            if model.roles.isEmpty {
                Text("draft.noRoles")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 150), spacing: 8)],
                    spacing: 8
                ) {
                    ForEach(model.roles) { role in
                        let selected = model.selectedRoleIDs.contains(role.id)
                        let label = Label(
                            role.name,
                            systemImage: selected ? "checkmark.circle.fill" : "circle"
                        )
                        .frame(maxWidth: .infinity, alignment: .leading)

                        if selected {
                            Button {
                                model.toggleRole(role.id)
                            } label: {
                                label
                            }
                            .buttonStyle(.borderedProminent)
                        } else {
                            Button {
                                model.toggleRole(role.id)
                            } label: {
                                label
                            }
                            .buttonStyle(.bordered)
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
