import SwiftUI

struct DraftView: View {
    @Environment(AppEnvironment.self) private var environment
    let workspace: Workspace
    @State private var prompt = ""

    private var model: WorkbenchViewModel { environment.workbench }

    var body: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 24)

            VStack(spacing: 18) {
                Image(systemName: "sparkles")
                    .font(.system(size: 34))
                    .foregroundStyle(.secondary)

                Text("draft.title \(workspace.displayName)")
                    .font(.title2)
                    .multilineTextAlignment(.center)

                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 150), spacing: 12)],
                    spacing: 12
                ) {
                    suggestion("draft.suggestion.explore", systemImage: "magnifyingglass", prompt: "draft.prompt.explore")
                    suggestion("draft.suggestion.build", systemImage: "hammer", prompt: "draft.prompt.build")
                    suggestion("draft.suggestion.review", systemImage: "checkmark.seal", prompt: "draft.prompt.review")
                    suggestion("draft.suggestion.fix", systemImage: "wrench.and.screwdriver", prompt: "draft.prompt.fix")
                }

                rolePicker

                if let message = model.errorMessage {
                    Text(message).foregroundStyle(.red)
                }
            }
            .frame(maxWidth: 720)

            Spacer(minLength: 24)
            composer
                .frame(maxWidth: 720)
        }
        .padding(24)
        .navigationTitle(workspace.displayName)
    }

    private func suggestion(
        _ title: LocalizedStringKey,
        systemImage: String,
        prompt promptKey: String
    ) -> some View {
        Button {
            prompt = String(localized: String.LocalizationValue(promptKey))
        } label: {
            VStack(alignment: .leading, spacing: 8) {
                Image(systemName: systemImage)
                    .foregroundStyle(.tint)
                Text(title)
                    .font(.callout)
                    .multilineTextAlignment(.leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(12)
            .frame(maxWidth: .infinity, minHeight: 78, alignment: .topLeading)
        }
        .buttonStyle(.bordered)
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
                        Button {
                            model.toggleRole(role.id)
                        } label: {
                            Label(
                                role.name,
                                systemImage: model.selectedRoleIDs.contains(role.id)
                                    ? "checkmark.circle.fill"
                                    : "circle"
                            )
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .buttonStyle(.bordered)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 10) {
            TextField("draft.placeholder", text: $prompt, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(2...6)

            Button("draft.send") {
                let text = prompt
                Task {
                    if await model.send(prompt: text) {
                        prompt = ""
                    }
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(prompt.isEmpty || model.selectedRoleIDs.isEmpty || model.isSending)
        }
    }
}
