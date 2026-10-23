import SwiftUI

struct CapabilityMultiSelect: View {
    let title: LocalizedStringKey
    let options: [CapabilityOption]
    let knownIDs: Set<String>
    @Binding var selection: [String]
    let isLoading: Bool
    let error: String?
    let searchPrompt: LocalizedStringKey
    let isEnabled: Bool
    let onRetry: () -> Void

    @State private var showPicker = false
    @State private var query = ""
    @State private var isExpanded = false

    init(
        title: LocalizedStringKey,
        options: [CapabilityOption],
        knownIDs: Set<String>,
        selection: Binding<[String]>,
        isLoading: Bool,
        error: String?,
        searchPrompt: LocalizedStringKey,
        isEnabled: Bool,
        onRetry: @escaping () -> Void
    ) {
        self.title = title
        self.options = options
        self.knownIDs = knownIDs
        self._selection = selection
        self.isLoading = isLoading
        self.error = error
        self.searchPrompt = searchPrompt
        self.isEnabled = isEnabled
        self.onRetry = onRetry
    }

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            if !selectedOptions.isEmpty {
                FlowLayout(spacing: 6, lineSpacing: 6) {
                    ForEach(selectedOptions) { option in
                        chip(option)
                    }
                }
            }
        } label: {
            HStack(spacing: 8) {
                Text(title)

                Spacer()

                Text("\(selection.count)")
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if isEnabled {
                    Button {
                        showPicker.toggle()
                    } label: {
                        Label("settings.agents.select", systemImage: "chevron.down")
                    }
                    .controlSize(.small)
                    .popover(isPresented: $showPicker, arrowEdge: .bottom) {
                        picker
                    }
                }
            }
            .help(
                isEnabled
                    ? LocalizedStringKey("settings.agents.select")
                    : LocalizedStringKey("settings.agents.mcp.disabled")
            )
        }
    }

    private func chip(_ option: CapabilityOption) -> some View {
        let missing = !knownIDs.contains(option.id)

        return HStack(spacing: 4) {
            Text(option.name)
                .lineLimit(1)

            Button {
                selection.removeAll { $0 == option.id }
            } label: {
                Image(systemName: "xmark")
                    .font(.caption2)
            }
            .buttonStyle(.borderless)
            .help("common.delete")
        }
        .font(.callout)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(Color(nsColor: .controlBackgroundColor), in: Capsule())
        .overlay(
            Capsule()
                .stroke(missing ? Color.orange : Color(nsColor: .separatorColor))
        )
    }

    private var picker: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 8) {
                TextField("", text: $query, prompt: Text(searchPrompt))
                    .labelsHidden()
                    .textFieldStyle(.roundedBorder)

                HStack(spacing: 12) {
                    Button("settings.agents.selectAll") {
                        for option in filtered where !selection.contains(option.id) {
                            selection.append(option.id)
                        }
                    }
                    Button("settings.agents.clearAll") {
                        selection.removeAll()
                    }
                    Spacer()
                    Text("\(selection.count)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.link)
                .font(.caption)
            }
            .padding(12)

            Divider()

            if isLoading, options.isEmpty {
                ProgressView().controlSize(.small)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error {
                VStack(alignment: .leading, spacing: 6) {
                    Text(error)
                        .font(.caption)
                        .foregroundStyle(.red)
                    Button("settings.agents.retry", action: onRetry)
                        .buttonStyle(.link)
                        .font(.caption)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .frame(maxHeight: .infinity, alignment: .top)
            } else if filtered.isEmpty {
                Text("settings.agents.capabilities.empty")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
            } else {
                List {
                    ForEach(filtered) { option in
                        Toggle(isOn: binding(for: option.id)) {
                            HStack(spacing: 4) {
                                Text(option.name)
                                if !knownIDs.contains(option.id) {
                                    Text("settings.agents.capabilityMissingShort")
                                        .font(.caption2)
                                        .foregroundStyle(.orange)
                                }
                            }
                        }
                        .toggleStyle(.checkbox)
                    }
                }
                .listStyle(.inset)
            }
        }
        .frame(width: 340, height: pickerHeight)
    }

    private var pickerHeight: CGFloat {
        if isLoading || error != nil || filtered.isEmpty {
            return 180
        }
        return 400
    }

    private var allOptions: [CapabilityOption] {
        var rows = options
        for id in selection where !rows.contains(where: { $0.id == id }) {
            rows.append(CapabilityOption(id: id, name: id, detail: nil))
        }
        return rows
    }

    private var selectedOptions: [CapabilityOption] {
        allOptions.filter { selection.contains($0.id) }
    }

    private var filtered: [CapabilityOption] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return allOptions }
        return allOptions.filter {
            $0.name.localizedCaseInsensitiveContains(needle)
                || $0.id.localizedCaseInsensitiveContains(needle)
        }
    }

    private func binding(for id: String) -> Binding<Bool> {
        Binding(
            get: { selection.contains(id) },
            set: { isOn in
                if isOn {
                    if !selection.contains(id) {
                        selection.append(id)
                    }
                } else {
                    selection.removeAll { $0 == id }
                }
            }
        )
    }
}

private struct FlowLayout: Layout {
    var spacing: CGFloat = 6
    var lineSpacing: CGFloat = 6

    func sizeThatFits(
        proposal: ProposedViewSize,
        subviews: Subviews,
        cache: inout ()
    ) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var lineHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x > 0, x + size.width > maxWidth {
                x = 0
                y += lineHeight + lineSpacing
                lineHeight = 0
            }
            x += size.width + spacing
            lineHeight = max(lineHeight, size.height)
        }

        return CGSize(width: proposal.width ?? max(0, x - spacing), height: y + lineHeight)
    }

    func placeSubviews(
        in bounds: CGRect,
        proposal: ProposedViewSize,
        subviews: Subviews,
        cache: inout ()
    ) {
        var x = bounds.minX
        var y = bounds.minY
        var lineHeight: CGFloat = 0

        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)

            if x > bounds.minX, x + size.width > bounds.maxX {
                x = bounds.minX
                y += lineHeight + lineSpacing
                lineHeight = 0
            }

            subview.place(
                at: CGPoint(x: x, y: y),
                anchor: .topLeading,
                proposal: ProposedViewSize(size)
            )

            x += size.width + spacing
            lineHeight = max(lineHeight, size.height)
        }
    }
}
