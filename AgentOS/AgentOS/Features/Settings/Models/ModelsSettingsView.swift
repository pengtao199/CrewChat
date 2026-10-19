import SwiftUI

struct ModelsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var accounts: [CCRAccount] = []
    @State private var isLoadingAccounts = false
    @State private var accountsError: String?

    private var model: ModelsSettingsViewModel { environment.models }

    private var selected: CCRProvider? {
        guard let selection else { return nil }
        return model.providers.first { $0.id == selection }
    }

    var body: some View {
        MasterDetailLayout {
            List(model.providers, selection: $selection) { provider in
                VStack(alignment: .leading, spacing: 2) {
                    Text(provider.name)
                    Text(provider.baseURL)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .tag(provider.id)
            }
        } right: {
            detail
        }
        .navigationTitle("settings.models")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await loadAccounts(forceRefresh: true) }
                } label: {
                    Label("settings.models.refresh", systemImage: "arrow.clockwise")
                }
                .disabled(selected == nil || isLoadingAccounts)
            }
        }
        .onChange(of: selection) { _, _ in
            Task { await loadAccounts(forceRefresh: false) }
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
        }
    }

    @ViewBuilder
    private var detail: some View {
        if let message = model.errorMessage {
            ContentUnavailableView {
                Label("settings.models.ccrDown", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                Button("settings.models.startCCR") {
                    Task { await model.startCCR() }
                }
                .disabled(model.isStarting)
            }
        } else if let provider = selected {
            providerDetail(provider)
        } else if model.providers.isEmpty {
            ContentUnavailableView(
                "settings.models.empty.title",
                systemImage: "cube.box",
                description: Text("settings.models.empty.description")
            )
        } else {
            SelectItemPlaceholder()
        }
    }

    private func providerDetail(_ provider: CCRProvider) -> some View {
        Form {
            Section("settings.models.basic") {
                LabeledContent("settings.models.type", value: provider.type)
                LabeledContent("settings.models.baseURL") {
                    Text(provider.baseURL)
                        .textSelection(.enabled)
                }
            }

            Section("settings.models.models") {
                ForEach(provider.models, id: \.self) { model in
                    Text(model)
                        .textSelection(.enabled)
                }
            }

            Section("settings.models.accounts") {
                if isLoadingAccounts {
                    ProgressView().controlSize(.small)
                } else if let accountsError {
                    Text(accountsError).foregroundStyle(.red)
                } else if accounts.isEmpty {
                    Text("settings.models.noAccounts")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(accounts) { account in
                        accountRow(account)
                    }
                }
            }

            Section("settings.models.ccr") {
                LabeledContent("settings.models.ccrURL", value: model.health?.url ?? "—")
                LabeledContent("settings.models.token") {
                    Text(model.health?.tokenConfigured == true ? "settings.models.token.ok" : "settings.models.token.missing")
                        .foregroundStyle(model.health?.tokenConfigured == true ? .green : .orange)
                }
            }
        }
        .formStyle(.grouped)
    }

    private func accountRow(_ account: CCRAccount) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(account.label)
                Spacer()
                Text(account.status)
                    .foregroundStyle(account.status == "ok" ? .green : .orange)
            }

            ForEach(account.meters) { meter in
                LabeledContent(meter.label) {
                    Text(meterText(meter))
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 2)
    }

    private func meterText(_ meter: CCRMeter) -> String {
        let remaining = meter.remaining.map { $0.formatted(.number.precision(.fractionLength(0...1))) } ?? "—"
        let limit = meter.limit.map { $0.formatted(.number.precision(.fractionLength(0...1))) } ?? "—"
        var text = "\(remaining) / \(limit) \(meter.unit)"
        if let window = meter.window, !window.isEmpty {
            text += " · \(window)"
        }
        return text
    }

    private func selectFirstIfNeeded() {
        guard let selection, model.providers.contains(where: { $0.id == selection }) else {
            self.selection = model.providers.first?.id
            Task { await loadAccounts(forceRefresh: false) }
            return
        }
    }

    private func loadAccounts(forceRefresh: Bool) async {
        guard let provider = selected else {
            accounts = []
            accountsError = nil
            return
        }

        isLoadingAccounts = true
        accountsError = nil

        switch await model.accounts(for: provider.name, forceRefresh: forceRefresh) {
        case let .success(rows):
            accounts = rows
        case let .failure(error):
            accounts = []
            accountsError = error.localizedDescription
        }

        isLoadingAccounts = false
    }
}
