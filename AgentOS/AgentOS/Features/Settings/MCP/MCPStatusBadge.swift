import SwiftUI

struct MCPStatusBadge: View {
    let availability: MCPAvailability

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "circle.fill")
                .font(.system(size: 6))
                .foregroundStyle(color)
            Text(label)
                .font(.caption2)
                .foregroundStyle(color)
        }
    }

    private var label: LocalizedStringKey {
        switch availability {
        case .disabled: "settings.mcp.disabled"
        case .checking: "settings.mcp.status.checking"
        case .available: "settings.mcp.status.available"
        case .unavailable: "settings.mcp.status.unavailable"
        case .unknown: "settings.mcp.status.unknown"
        }
    }

    private var color: Color {
        switch availability {
        case .disabled: .secondary
        case .checking: .orange
        case .available: .green
        case .unavailable: .red
        case .unknown: .secondary
        }
    }
}
