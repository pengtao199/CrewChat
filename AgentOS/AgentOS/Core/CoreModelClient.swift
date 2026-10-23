import Foundation

final class CoreModelClient: CoreModel, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func health() async throws -> ModelHealth {
        let health: CoreModelHealth = try await provider.client().send("model_health", body: EmptyBody())
        return ModelHealth(url: health.url, tokenConfigured: health.tokenConfigured)
    }

    func start() async throws {
        try await provider.client().sendIgnoring("model_supervisor_start", body: EmptyBody())
    }

    func providers() async throws -> [ModelProvider] {
        let config: CoreModelConfig = try await provider.client().send(
            "model_call",
            body: CoreModelCall(method: "getConfig", args: [])
        )
        // Models the user switched off live in a core-owned file, not in the
        // Model provider (its loader would drop an unknown field).
        let disabled: [String: [String]] = (try? await provider.client().send(
            "model_disabled_models",
            body: EmptyBody()
        )) ?? [:]
        return config.providers.map { provider in
            let key = provider.id ?? provider.name
            let removed = disabled[key] ?? disabled[provider.name] ?? []
            return provider.domain(disabledModels: removed)
        }
    }

    func presets() async throws -> [ModelProviderPreset] {
        let rows: [CoreModelPreset] = try await provider.client().send(
            "model_call",
            body: CoreModelCall(method: "getProviderPresets", args: [])
        )
        return rows.map(\.domain)
    }

    func accounts(provider name: String, forceRefresh: Bool) async throws -> [ModelAccount] {
        let rows: [CoreModelAccount] = try await provider.client().send(
            "model_call",
            body: CoreModelCall(
                method: "getProviderAccountSnapshots",
                args: [.string(name), .object(forceRefresh ? ["forceRefresh": true] : [:])]
            )
        )
        return rows.map(\.domain)
    }

    func saveProvider(_ draft: ModelProviderDraft) async throws -> ModelProvider {
        let saved: CoreModelProvider = try await provider.client().send(
            "model_provider_save",
            body: CoreModelProviderSave(draft)
        )
        return saved.domain(disabledModels: [])
    }

    func deleteProvider(id: String) async throws {
        try await provider.client().sendIgnoring(
            "model_provider_delete",
            body: CoreModelIdBody(id: id)
        )
    }

    func setModelEnabled(providerID: String, model: String, enabled: Bool) async throws {
        try await provider.client().sendIgnoring(
            "model_provider_set_model",
            body: CoreModelModelToggle(providerId: providerID, model: model, enabled: enabled)
        )
    }

    func setCredentialEnabled(
        providerID: String,
        credentialID: String,
        enabled: Bool
    ) async throws {
        try await provider.client().sendIgnoring(
            "model_provider_set_credential",
            body: CoreModelCredentialToggle(
                providerId: providerID,
                credentialId: credentialID,
                enabled: enabled
            )
        )
    }

    func probeProvider(baseURL: String, apiKey: String?) async throws -> ModelProbeResult {
        let result: CoreModelProbeResult = try await provider.client().send(
            "model_provider_probe",
            body: CoreModelProbeBody(baseURL: baseURL, apiKey: apiKey)
        )
        return ModelProbeResult(
            models: result.models,
            baseURL: result.normalizedBaseUrl,
            protocolName: result.detectedProtocol
        )
    }
}

private extension CoreModelProvider {
    func domain(disabledModels: [String]) -> ModelProvider {
        var resolved = (credentials ?? []).map(\.domain)
        // Older Model configs keep the key at the provider level. Surface it as
        // one credential so the list, the masked display and the probe share a
        // single path.
        if resolved.isEmpty, let apiKey, !apiKey.isEmpty {
            resolved = [
                ModelCredential(
                    id: "api_key",
                    name: "Key 1",
                    apiKey: apiKey,
                    enabled: true,
                    priority: nil,
                    weight: nil,
                    limits: nil,
                    isSynthetic: true
                )
            ]
        }

        return ModelProvider(
            id: id ?? name,
            name: name,
            type: type ?? "—",
            baseURL: baseURL ?? "—",
            enabled: enabled ?? true,
            models: models ?? [],
            credentials: resolved,
            disabledModels: disabledModels
        )
    }
}

private extension CoreModelCredential {
    var domain: ModelCredential {
        ModelCredential(
            id: id ?? name ?? label ?? UUID().uuidString,
            name: name ?? label ?? "Key",
            apiKey: apiKey ?? "",
            enabled: enabled ?? true,
            priority: priority,
            weight: weight,
            limits: limits,
            isSynthetic: false
        )
    }

}

private extension CoreModelAccount {
    var domain: ModelAccount {
        ModelAccount(
            id: credentialId ?? credentialLabel ?? UUID().uuidString,
            label: credentialLabel ?? credentialId ?? "Key",
            status: status,
            meters: meters.map(\.domain)
        )
    }
}

private extension CoreModelMeter {
    var domain: ModelMeter {
        ModelMeter(
            id: id,
            label: label,
            unit: unit ?? "",
            remaining: remaining,
            limit: limit,
            used: used,
            window: window,
            resetAt: resetAt
        )
    }
}
