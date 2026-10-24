import SwiftUI

struct Composer: View {
    @Binding var text: String
    @Binding var focusRequested: Bool
    let placeholder: LocalizedStringKey
    let members: [ConversationMember]
    let isRunning: Bool
    let canSend: Bool
    let onSend: () -> Void
    let onStop: () -> Void

    @State private var isFocused = false
    @State private var editorHeight: CGFloat = 38
    @State private var showMentions = false
    @State private var mentionQuery = ""

    var body: some View {
        Group {
            if #available(macOS 26.0, *) {
                panel
                    .glassEffect(
                        .regular,
                        in: RoundedRectangle(cornerRadius: 22, style: .continuous)
                    )
            } else {
                panel
                    .background(
                        .ultraThinMaterial,
                        in: RoundedRectangle(cornerRadius: 22, style: .continuous)
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 22, style: .continuous)
                            .strokeBorder(.separator.opacity(0.7))
                    )
            }
        }
        .shadow(color: .black.opacity(0.10), radius: 14, y: 6)
        .onChange(of: text) { _, value in
            if let context = mentionContext(value) {
                mentionQuery = context
                showMentions = true
            } else {
                showMentions = false
            }
        }
    }

    private var panel: some View {
        VStack(spacing: 4) {
            ZStack(alignment: .topLeading) {
                if text.isEmpty, !isFocused {
                    Text(placeholder)
                        .foregroundStyle(.tertiary)
                        .padding(.top, 3)
                        .allowsHitTesting(false)
                }

                ChatTextEditor(
                    text: $text,
                    isFocused: $isFocused,
                    focusRequested: focusRequested,
                    onFocusHandled: { focusRequested = false },
                    onSubmit: {
                        if showMentions, let first = filteredMembers.first {
                            insert(first)
                        } else if canSend {
                            onSend()
                        }
                    },
                    onHeightChange: { height in
                        let clamped = min(max(ceil(height), 38), 160)
                        if abs(clamped - editorHeight) > 0.5 {
                            editorHeight = clamped
                        }
                    }
                )
                .frame(height: editorHeight)
            }

            HStack(spacing: 8) {
                Button {
                } label: {
                    Image(systemName: "plus")
                }
                .buttonStyle(.borderless)
                .disabled(true)
                .help("composer.attach")

                if !members.isEmpty {
                    Button {
                        mentionQuery = ""
                        showMentions = true
                    } label: {
                        Image(systemName: "at")
                    }
                    .buttonStyle(.borderless)
                    .help("conversation.mention.help")
                    .popover(isPresented: $showMentions, arrowEdge: .top) {
                        mentionPicker
                    }
                }

                Spacer()

                if isRunning {
                    Button(action: onStop) {
                        Image(systemName: "stop.fill")
                            .font(.system(size: 11, weight: .bold))
                            .frame(width: 22, height: 22)
                    }
                    .buttonStyle(.borderedProminent)
                    .buttonBorderShape(.circle)
                    .tint(.red)
                    .help("composer.stop")
                } else {
                    Button(action: onSend) {
                        Image(systemName: "arrow.up")
                            .font(.system(size: 13, weight: .semibold))
                            .frame(width: 22, height: 22)
                    }
                    .buttonStyle(.borderedProminent)
                    .buttonBorderShape(.circle)
                    .disabled(!canSend)
                    .help("composer.send")
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 9)
        .padding(.bottom, 7)
    }

    private var mentionPicker: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("conversation.mention.title")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)

            Divider()

            if filteredMembers.isEmpty {
                Text("conversation.mention.empty")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(12)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(filteredMembers) { member in
                            Button {
                                insert(member)
                            } label: {
                                HStack(spacing: 8) {
                                    AgentAvatar(
                                        name: member.name,
                                        size: 22,
                                        state: member.state,
                                        isLeader: member.isLeader
                                    )
                                    Text(member.name)
                                        .lineLimit(1)
                                    Spacer()
                                    if member.isLeader {
                                        Text("conversation.member.leader")
                                            .font(.caption2)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                .padding(.horizontal, 10)
                                .padding(.vertical, 6)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }
        }
        .frame(width: 280, height: filteredMembers.isEmpty ? 120 : 300)
    }

    private var filteredMembers: [ConversationMember] {
        let needle = mentionQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return members }
        return members.filter {
            $0.name.localizedCaseInsensitiveContains(needle)
                || $0.roleID.localizedCaseInsensitiveContains(needle)
        }
    }

    private func mentionContext(_ value: String) -> String? {
        guard let at = value.lastIndex(of: "@") else { return nil }
        if at > value.startIndex {
            let previous = value[value.index(before: at)]
            guard previous.isWhitespace || previous.isNewline else { return nil }
        }
        let query = String(value[value.index(after: at)...])
        guard query.allSatisfy({ !$0.isWhitespace && !$0.isNewline }) else { return nil }
        return query
    }

    private func insert(_ member: ConversationMember) {
        if let at = text.lastIndex(of: "@"),
           mentionContext(text) != nil {
            text = String(text[..<at]) + "@\(member.name) "
        } else if text.isEmpty || text.hasSuffix(" ") || text.hasSuffix("\n") {
            text += "@\(member.name) "
        } else {
            text += " @\(member.name) "
        }
        mentionQuery = ""
        showMentions = false
        focusRequested = true
    }
}
