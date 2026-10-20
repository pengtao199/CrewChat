import SwiftUI

struct Composer: View {
    @Binding var text: String
    @Binding var focusRequested: Bool
    let placeholder: LocalizedStringKey
    let isSending: Bool
    let onSend: () -> Void

    @FocusState private var focused: Bool

    var body: some View {
        VStack(spacing: 8) {
            TextField(placeholder, text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...6)
                .focused($focused)

            HStack(spacing: 8) {
                Button {
                } label: {
                    Image(systemName: "plus")
                }
                .buttonStyle(.borderless)
                .disabled(true)
                .help("composer.attach")

                Spacer()

                Button(action: onSend) {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 13, weight: .semibold))
                        .frame(width: 22, height: 22)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.circle)
                .disabled(!canSend)
            }
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color(nsColor: .textBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(Color(nsColor: .separatorColor))
        )
        .onChange(of: focusRequested) { _, requested in
            if requested {
                focused = true
                focusRequested = false
            }
        }
    }

    private var canSend: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !isSending
    }
}
