import SwiftUI

struct MCPDetailForm: View {
    @Binding var draft: MCPServerDraft
    @Binding var advancedExpanded: Bool

    let health: MCPServerHealth?
    let isSaving: Bool
    let loadError: String?
    let saveError: String?

    var body: some View {
        Form {
            if let loadError {
                Section {
                    Text(loadError)
                        .foregroundStyle(.red)
                }
            }

            if isSaving {
                Section {
                    HStack(spacing: 8) {
                        ProgressView()
                            .controlSize(.small)
                        Text("settings.mcp.saving")
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Section("settings.mcp.basic") {
                LabeledContent("settings.mcp.id") {
                    Text(draft.id)
                        .font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                }

                LabeledContent("settings.mcp.status") {
                    MCPStatusBadge(availability: availability)
                }

                if availability == .available, let count = health?.toolCount {
                    LabeledContent("settings.mcp.tools") {
                        Text("\(count)")
                    }
                }

                Picker("settings.mcp.transport", selection: $draft.transport) {
                    ForEach(transports, id: \.self) { transport in
                        Text(verbatim: transport).tag(transport)
                    }
                }

                Toggle("settings.mcp.enabled", isOn: $draft.enabled)
            }

            if availability == .unavailable, let error = health?.error {
                Section("settings.mcp.status.error") {
                    Text(error)
                        .font(.callout)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                }
            }

            if draft.isRemote {
                remoteSection
            } else {
                stdioSection
            }

            if let validation = draft.validationMessage {
                Section {
                    Text(validation)
                        .foregroundStyle(.orange)
                }
            }

            if let saveError {
                Section {
                    Text(saveError)
                        .foregroundStyle(.red)
                }
            }

            advancedSection
        }
        .formStyle(.grouped)
    }

    private var stdioSection: some View {
        Section {
            TextField("settings.mcp.command", text: $draft.command)
                .font(.system(.body, design: .monospaced))

            TextEditor(text: $draft.argsText)
                .font(.system(.callout, design: .monospaced))
                .frame(minHeight: 64)

            TextEditor(text: $draft.envText)
                .font(.system(.callout, design: .monospaced))
                .frame(minHeight: 64)
        } header: {
            Text("settings.mcp.stdio")
        } footer: {
            VStack(alignment: .leading, spacing: 2) {
                Text("settings.mcp.argsHint")
                Text("settings.mcp.envHint")
            }
            .foregroundStyle(.secondary)
        }
    }

    private var remoteSection: some View {
        Section {
            TextField("settings.mcp.url", text: $draft.url)
                .font(.system(.body, design: .monospaced))

            TextEditor(text: $draft.headersText)
                .font(.system(.callout, design: .monospaced))
                .frame(minHeight: 64)

            TextField("settings.mcp.bearerToken", text: $draft.bearerToken)
                .font(.system(.body, design: .monospaced))
        } header: {
            Text("settings.mcp.remote")
        } footer: {
            VStack(alignment: .leading, spacing: 2) {
                Text("settings.mcp.headersHint")
                Text("settings.mcp.bearerTokenHint")
            }
            .foregroundStyle(.secondary)
        }
    }

    private var advancedSection: some View {
        Section {
            DisclosureGroup(isExpanded: $advancedExpanded) {
                ScrollView {
                    Text(draft.previewSpec.prettyText)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(10)
                }
                .frame(minHeight: 160, maxHeight: 320)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(.separator)
                )
            } label: {
                Text("settings.mcp.advanced")
            }
        } footer: {
            Text("settings.mcp.advancedHint")
                .foregroundStyle(.secondary)
        }
    }

    private var transports: [String] {
        var values = ["stdio", "http", "sse"]
        if !values.contains(draft.transport) {
            values.append(draft.transport)
        }
        return values
    }

    private var availability: MCPAvailability {
        MCPAvailability.resolve(enabled: draft.enabled, health: health)
    }
}
