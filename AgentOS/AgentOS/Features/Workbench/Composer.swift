import SwiftUI

struct Composer: View {
    @Binding var text: String
    @Binding var focusRequested: Bool
    let placeholder: LocalizedStringKey
    let isRunning: Bool
    let canSend: Bool
    let onSend: () -> Void
    let onStop: () -> Void

    @State private var isFocused = false

    var body: some View {
        VStack(spacing: 6) {
            ZStack(alignment: .topLeading) {
                if text.isEmpty, !isFocused {
                    Text(placeholder)
                        .foregroundStyle(.tertiary)
                        .padding(.top, 4)
                        .allowsHitTesting(false)
                }

                ChatTextEditor(
                    text: $text,
                    isFocused: $isFocused,
                    focusRequested: focusRequested,
                    onFocusHandled: { focusRequested = false },
                    onSubmit: {
                        if canSend {
                            onSend()
                        }
                    }
                )
                .frame(maxWidth: .infinity, minHeight: 40, maxHeight: 140)
            }

            HStack(spacing: 8) {
                Button {
                } label: {
                    Image(systemName: "plus")
                }
                .buttonStyle(.borderless)
                .disabled(true)
                .help("composer.attach")

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
                }
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
    }
}
