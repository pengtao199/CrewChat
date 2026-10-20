import AppKit
import SwiftUI

struct ChatTextEditor: NSViewRepresentable {
    @Binding var text: String
    let focusRequested: Bool
    let onFocusHandled: () -> Void
    let onSubmit: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(self)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true

        guard let textView = scroll.documentView as? NSTextView else { return scroll }
        textView.delegate = context.coordinator
        textView.isRichText = false
        textView.drawsBackground = false
        textView.font = .systemFont(ofSize: NSFont.systemFontSize)
        textView.textContainerInset = NSSize(width: 0, height: 3)
        textView.isVerticallyResizable = true
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        textView.string = text
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let textView = scroll.documentView as? NSTextView else { return }

        if textView.string != text {
            textView.string = text
        }

        if focusRequested, !context.coordinator.didFocus {
            context.coordinator.didFocus = true
            DispatchQueue.main.async {
                textView.window?.makeFirstResponder(textView)
                onFocusHandled()
            }
        } else if !focusRequested {
            context.coordinator.didFocus = false
        }
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        private let parent: ChatTextEditor
        var didFocus = false

        init(_ parent: ChatTextEditor) {
            self.parent = parent
        }

        func textDidChange(_ notification: Notification) {
            guard let textView = notification.object as? NSTextView else { return }
            parent.text = textView.string
            textView.scrollRangeToVisible(textView.selectedRange())
        }

        func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            guard selector == #selector(NSResponder.insertNewline(_:)) else { return false }

            if NSEvent.modifierFlags.contains(.shift) {
                textView.insertNewlineIgnoringFieldEditor(nil)
                return true
            }

            parent.onSubmit()
            return true
        }
    }
}
