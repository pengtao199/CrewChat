import SwiftUI

struct ModelsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var accounts: [CCRAccount] = []
    @State private var isLoadingAccounts = false
    @State private var accountsError: String?
    @State private var revealed: Set<String> = []
    @State private var modelsExpanded = true
    @State private var accountsExpanded = true
    @State private var editor: EditorTarget?
    @State private var confirmDelete = false
    @State private var actionError: String?
    @State private var query = ""

    private var model: ModelsSettingsViewModel { environment.models }

    private var selected: CCRProvider? {
        guard let selection else { return nil }
        return model.providers.first { $0.id == selection }
    }

    private var filteredProviders: [CCRProvider] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return model.providers }
        return model.providers.filter {
            $0.name.localizedCaseInsensitiveContains(needle)
                || $0.baseURL.localizedCaseInsensitiveContains(needle)
        }
    }

    var body: some View {
        MasterDetailLayout {
            List(filteredProviders, selection: $selection) { provider in
                VStack(alignment: .leading, spacing: 2) {
                    Text(provider.name)
                    Text(provider.baseURL)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .tag(provider.id)
            }
            .searchable(text: $query, placement: .sidebar, prompt: Text("settings.models.search"))
        } right: {
            detail
        }
        .navigationTitle("settings.models")
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button {
                    editor = .create
                } label: {
                    Label("settings.models.add", systemImage: "plus")
                }

                Button {
                    if let selected {
                        editor = .edit(selected)
                    }
                } label: {
                    Label("common.edit", systemImage: "pencil")
                }
                .disabled(selected == nil)

                Button(role: .destructive) {
                    confirmDelete = true
                } label: {
                    Label("common.delete", systemImage: "trash")
                }
                .disabled(selected == nil)

                Button {
                    Task {
                        await model.load()
                        await loadAccounts(forceRefresh: true)
                    }
                } label: {
                    Label("settings.models.refresh", systemImage: "arrow.clockwise")
                }
                .disabled(selected == nil || isLoadingAccounts || model.isLoading)
            }
        }
        .sheet(item: $editor) { target in
            switch target {
            case .create:
                ProviderEditorSheet(existing: nil, model: model) { saved in
                    if let saved {
                        selection = saved.id
                    }
                }
            case let .edit(provider):
                ProviderEditorSheet(existing: provider, model: model) { saved in
                    if let saved {
                        selection = saved.id
                    } else {
                        selection = nil
                        selectFirstIfNeeded()
                    }
                }
            }
        }
        .alert(
            "settings.models.delete.title",
            isPresented: $confirmDelete,
            presenting: selected
        ) { provider in
            Button("common.delete", role: .destructive) {
                Task { await delete(provider) }
            }
            Button("common.cancel", role: .cancel) {}
        } message: { provider in
            Text(String(format: String(localized: "settings.models.delete.message"), provider.name))
        }
        .task {
            await model.load()
            selectFirstIfNeeded()
        }
        .task(id: selection) {
            revealed.removeAll()
            await loadAccounts(forceRefresh: false)
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

                Button("settings.models.reload") {
                    Task {
                        await model.load()
                        await loadAccounts(forceRefresh: true)
                    }
                }
            }
        } else if let provider = selected {
            providerDetail(provider)
        } else if model.providers.isEmpty {
            ContentUnavailableView {
                Label("settings.models.empty.title", systemImage: "cube.box")
            } description: {
                Text("settings.models.empty.description")
            } actions: {
                Button("settings.models.add") {
                    editor = .create
                }
            }
        } else {
            SelectItemPlaceholder()
        }
    }

    private func providerDetail(_ provider: CCRProvider) -> some View {
        Form {
            if let actionError {
                Section {
                    Text(actionError)
                        .foregroundStyle(.red)
                }
            }

            Section("settings.models.basic") {
                LabeledContent("settings.models.name", value: provider.name)
                LabeledContent("settings.models.type", value: provider.type)
                LabeledContent("settings.models.enabled") {
                    Label(
                        provider.enabled ? "common.yes" : "common.no",
                        systemImage: provider.enabled ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
                    )
                    .foregroundStyle(provider.enabled ? .green : .orange)
                }
                LabeledContent("settings.models.baseURL") {
                    Text(provider.baseURL)
                        .textSelection(.enabled)
                }
            }

            Section("settings.models.credentials") {
                if provider.credentials.isEmpty {
                    Text("settings.models.noCredentials")
                        .foregroundStyle(.secondary)
                }
                ForEach(provider.credentials) { credential in
                    LabeledContent(credential.name) {
                        HStack(spacing: 8) {
                            Text(revealed.contains(credential.id) ? credential.apiKey : credential.masked)
                                .monospaced()
                                .textSelection(.enabled)
                            Button {
                                if revealed.contains(credential.id) {
                                    revealed.remove(credential.id)
                                } else {
                                    revealed.insert(credential.id)
                                }
                            } label: {
                                Image(systemName: revealed.contains(credential.id) ? "eye.slash" : "eye")
                            }
                            .buttonStyle(.borderless)
                            .help(revealed.contains(credential.id) ? "settings.models.hideKey" : "settings.models.showKey")
                        }
                    }
                }
            }

            Section("settings.models.accounts", isExpanded: $accountsExpanded) {
                if isLoadingAccounts {
                    ProgressView().controlSize(.small)
                } else if let accountsError {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(accountsError).foregroundStyle(.red)
                        Button("settings.models.retry") {
                            Task { await loadAccounts(forceRefresh: true) }
                        }
                        .buttonStyle(.link)
                        .font(.caption)
                    }
                } else if accounts.isEmpty {
                    Text("settings.models.noAccounts")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(accounts) { account in
                        accountRow(account)
                    }
                }
            }

            Section("settings.models.models", isExpanded: $modelsExpanded) {
                ForEach(provider.models, id: \.self) { model in
                    Text(model)
                        .textSelection(.enabled)
                }
            }

            Section("settings.models.ccr") {
                LabeledContent("settings.models.ccrURL", value: model.health?.url ?? "—")
                LabeledContent("settings.models.token") {
                    Text(tokenKey)
                        .foregroundStyle(tokenColor)
                }
            }

            Section {
                HStack {
                    Button("common.edit") {
                        editor = .edit(provider)
                    }
                    Button("common.delete", role: .destructive) {
                        confirmDelete = true
                    }
                    Spacer()
                }
            }
        }
        .formStyle(.grouped)
    }

    private var tokenKey: LocalizedStringKey {
        model.health?.tokenConfigured == true
            ? "settings.models.token.ok"
            : "settings.models.token.missing"
    }

    private var tokenColor: Color {
        model.health?.tokenConfigured == true ? .green : .orange
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

    private func delete(_ provider: CCRProvider) async {
        actionError = nil
        do {
            try await model.deleteProvider(id: provider.id)
            selection = nil
            selectFirstIfNeeded()
        } catch {
            actionError = error.localizedDescription
        }
    }
}

private enum EditorTarget: Identifiable {
    case create
    case edit(CCRProvider)

    var id: String {
        switch self {
        case .create: "create"
        case let .edit(provider): provider.id
        }
    }
}
