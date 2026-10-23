import SwiftUI

enum AgentDocument: String, Identifiable {
    case soul
    case rules

    var id: String { rawValue }

    var titleKey: LocalizedStringKey {
        switch self {
        case .soul: "settings.agents.soul"
        case .rules: "settings.agents.rules"
        }
    }
}

struct AgentPersonaFields: View {
    @Binding var soul: String
    @Binding var rules: String
    @Binding var soulExpanded: Bool
    @Binding var rulesExpanded: Bool
    let onEdit: (AgentDocument) -> Void

    var body: some View {
        group(
            title: "settings.agents.soul",
            text: $soul,
            expanded: $soulExpanded,
            document: .soul
        )
        group(
            title: "settings.agents.rules",
            text: $rules,
            expanded: $rulesExpanded,
            document: .rules
        )
    }

    private func group(
        title: LocalizedStringKey,
        text: Binding<String>,
        expanded: Binding<Bool>,
        document: AgentDocument
    ) -> some View {
        DisclosureGroup(isExpanded: expanded) {
            if text.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                Text("settings.agents.persona.empty")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ScrollView {
                    StructuredText(markdown: text.wrappedValue)
                        .textual.structuredTextStyle(.gitHub)
                        .textual.textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 80, maxHeight: 180)
            }
        } label: {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)

                    if let summary = firstLine(text.wrappedValue) {
                        Text(summary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }

                Spacer()

                Button("settings.agents.edit") {
                    onEdit(document)
                }
                .buttonStyle(.borderless)
                .font(.caption)
            }
        }
    }

    private func firstLine(_ text: String) -> String? {
        text.split(separator: "\n", omittingEmptySubsequences: false)
            .lazy
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .first { !$0.isEmpty }
    }
}
