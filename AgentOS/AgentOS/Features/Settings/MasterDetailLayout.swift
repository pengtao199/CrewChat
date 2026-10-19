import SwiftUI

struct MasterDetailLayout<Left: View, Right: View>: View {
    @ViewBuilder let left: Left
    @ViewBuilder let right: Right

    var body: some View {
        HSplitView {
            left
                .frame(minWidth: 200, idealWidth: 240, maxWidth: 320)
            right
                .frame(minWidth: 380, maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

struct SelectItemPlaceholder: View {
    var body: some View {
        ContentUnavailableView(
            "settings.select.title",
            systemImage: "sidebar.left",
            description: Text("settings.select.description")
        )
    }
}
