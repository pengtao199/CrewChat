import Foundation
import SwiftUI

struct ModelsSettingsView: View {
    @Environment(AppEnvironment.self) private var environment
    @State private var selection: String?
    @State private var accounts: [CCRAccount] = []
    @State private var isLoadingAccounts = false
    @State private var accountsError: String?
    @State private var togglingModel: String?
    @State private var togglingCredential: String?
    @State private var editor: EditorTarget?
    @State private var confirmDelete = false
    @State private var actionError: String?

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
            actionError = nil
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

            Section {
                if provider.credentials.isEmpty {
                    Text("settings.models.noCredentials")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(
                        Array(provider.credentials.enumerated()),
                        id: \.element.id
                    ) { index, credential in
                        HStack(spacing: 8) {
                            Text("\(index + 1)")
                                .monospacedDigit()
                                .foregroundStyle(.secondary)
                                .frame(width: 18, alignment: .trailing)
                            Text(credential.name)
                            Text(credential.keyTail)
                                .font(.caption)
                                .monospaced()
                                .foregroundStyle(.secondary)
                            Spacer(minLength: 8)
                            Toggle(
                                "",
                                isOn: Binding(
                                    get: { credential.enabled },
                                    set: { enabled in
                                        Task {
                                            await setCredentialEnabled(
                                                provider,
                                                credential,
                                                enabled
                                            )
                                        }
                                    }
                                )
                            )
                            .labelsHidden()
                            .toggleStyle(.switch)
                            .controlSize(.small)
                            .disabled(
                                credential.isSynthetic
                                    || togglingCredential == credential.id
                            )
                        }
                    }
                }
            } header: {
                Text("settings.models.credentials")
            }

            Section {
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
            } header: {
                HStack(spacing: 8) {
                    Text("settings.models.usage")
                    Spacer()
                    Button {
                        Task { await loadAccounts(forceRefresh: true) }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .disabled(isLoadingAccounts)
                    .help("settings.models.refresh")
                }
            }

            Section {
                if listedModels(for: provider).isEmpty {
                    Text("settings.models.modelsEmpty")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(listedModels(for: provider), id: \.self) { modelID in
                        HStack(spacing: 8) {
                            Text(modelID)
                                .font(.callout)
                                .monospaced()
                                .textSelection(.enabled)
                            Spacer(minLength: 8)
                            Toggle(
                                "",
                                isOn: Binding(
                                    get: { provider.models.contains(modelID) },
                                    set: { enabled in
                                        Task {
                                            await setModelEnabled(
                                                provider,
                                                modelID: modelID,
                                                enabled
                                            )
                                        }
                                    }
                                )
                            )
                            .labelsHidden()
                            .toggleStyle(.switch)
                            .controlSize(.small)
                            .disabled(togglingModel == modelID)
                        }
                    }
                }
            } header: {
                HStack(spacing: 4) {
                    Text("settings.models.models")
                    Text(verbatim: "(\(listedModels(for: provider).count))")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
    }

    private func listedModels(for provider: CCRProvider) -> [String] {
        Array(Set(provider.models + provider.disabledModels)).sorted()
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
                LabeledContent(meterLabel(meter)) {
                    Text(meterText(meter))
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 2)
    }

    private func meterLabel(_ meter: CCRMeter) -> String {
        guard let window = meter.window, !window.isEmpty else {
            return meter.label
        }
        return "\(meter.label) (\(window))"
    }

    private func meterText(_ meter: CCRMeter) -> String {
        let unit = meter.unit
        let text: String

        if let remaining = meter.remaining {
            if let limit = meter.limit {
                text = "\(numberText(remaining))/\(numberText(limit)) \(unit)"
            } else {
                text = "\(numberText(remaining)) \(unit)"
            }
        } else if let used = meter.used {
            text = "\(numberText(used)) \(unit)"
        } else {
            text = ""
        }

        return text.trimmingCharacters(in: .whitespaces)
    }

    private func numberText(_ value: Double) -> String {
        value == value.rounded()
            ? String(Int(value))
            : String(format: "%.1f", value)
    }

    private func setModelEnabled(
        _ provider: CCRProvider,
        modelID: String,
        _ enabled: Bool
    ) async {
        togglingModel = modelID
        actionError = nil

        do {
            try await model.setModelEnabled(
                providerID: provider.id,
                modelID: modelID,
                enabled: enabled
            )
        } catch {
            actionError = error.localizedDescription
            await model.load()
        }

        togglingModel = nil
    }

    private func setCredentialEnabled(
        _ provider: CCRProvider,
        _ credential: CCRCredential,
        _ enabled: Bool
    ) async {
        togglingCredential = credential.id
        actionError = nil

        do {
            try await model.setCredentialEnabled(
                providerID: provider.id,
                credentialID: credential.id,
                enabled: enabled
            )
        } catch {
            actionError = error.localizedDescription
            await model.load()
        }

        togglingCredential = nil
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
