import SwiftUI

enum MentionParser {
    static func memberIDs(in text: String, members: [ConversationMember]) -> [String] {
        matches(in: text, members: members).map(\.roleID)
    }

    static func matches(
        in text: String,
        members: [ConversationMember]
    ) -> [(roleID: String, range: Range<String.Index>)] {
        guard text.contains("@") else { return [] }
        var result: [(String, Range<String.Index>)] = []
        for member in members.sorted(by: { $0.name.count > $1.name.count }) {
            let token = "@\(member.name)"
            var searchStart = text.startIndex
            while let range = text.range(of: token, range: searchStart..<text.endIndex) {
                if !result.contains(where: { $0.1.overlaps(range) }) {
                    result.append((member.roleID, range))
                }
                searchStart = range.upperBound
            }
        }
        return result
            .sorted { $0.1.lowerBound < $1.1.lowerBound }
            .map { ($0.0, $0.1) }
    }
}

struct MentionText: View {
    let text: String
    let members: [ConversationMember]

    var body: some View {
        segments
            .textSelection(.enabled)
    }

    private var segments: Text {
        let matches = MentionParser.matches(in: text, members: members)
        guard !matches.isEmpty else { return Text(text) }

        var output = Text("")
        var cursor = text.startIndex
        for match in matches {
            if cursor < match.range.lowerBound {
                output = output + Text(String(text[cursor..<match.range.lowerBound]))
            }
            output = output + Text(String(text[match.range])).foregroundStyle(.tint).bold()
            cursor = match.range.upperBound
        }
        if cursor < text.endIndex {
            output = output + Text(String(text[cursor...]))
        }
        return output
    }
}

struct MentionBadges: View {
    let text: String
    let members: [ConversationMember]

    var body: some View {
        let matched = MentionParser.matches(in: text, members: members)
        if !matched.isEmpty {
            HStack(spacing: 4) {
                ForEach(Array(matched.enumerated()), id: \.offset) { _, item in
                    if let member = members.first(where: { $0.roleID == item.roleID }) {
                        Text(verbatim: "@\(member.name)")
                            .font(.caption2)
                            .foregroundStyle(.tint)
                    }
                }
            }
        }
    }
}
