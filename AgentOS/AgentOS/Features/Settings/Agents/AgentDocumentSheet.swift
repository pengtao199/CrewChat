import SwiftUI

struct AgentDocumentSheet: View {
    let title: LocalizedStringKey
    @Binding var text: String
    let onDone: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(title)
                    .font(.title3.weight(.semibold))
                Spacer()
            }
            .padding(.horizontal, 16)
            .padding(.top, 14)

            TextEditor(text: $text)
                .font(.system(.body, design: .monospaced))
                .padding(12)
                .background(Color(nsColor: .textBackgroundColor))
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(.separator)
                )
                .padding(16)

            Divider()

            HStack {
                Spacer()
                Button("common.done") {
                    onDone()
                    dismiss()
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
            .padding(14)
        }
        .frame(minWidth: 640, minHeight: 520)
    }
}
