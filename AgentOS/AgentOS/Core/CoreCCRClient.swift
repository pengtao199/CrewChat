import Foundation

final class CoreCCRClient: CoreCCR, @unchecked Sendable {
    private let provider: CoreClientProvider

    init(provider: CoreClientProvider) {
        self.provider = provider
    }

    func health() async throws -> CCRHealth {
        let health: CoreCCRHealth = try await provider.client().send("ccr_health", body: EmptyBody())
        return CCRHealth(url: health.url, tokenConfigured: health.tokenConfigured)
    }

    func start() async throws {
        try await provider.client().sendIgnoring("ccr_supervisor_start", body: EmptyBody())
    }

    func providers() async throws -> [CCRProvider] {
        let config: CoreCCRConfig = try await provider.client().send(
            "ccr_call",
            body: CoreCCRCall(method: "getConfig", args: [])
        )
        // Models the user switched off live in a core-owned file, not in the
        // CCR provider (its loader would drop an unknown field).
        let disabled: [String: [String]] = (try? await provider.client().send(
            "ccr_disabled_models",
            body: EmptyBody()
        )) ?? [:]
        return config.providers.map {
            $0.domain(disabledModels: disabled[$0.id ?? $0.name] ?? [])
        }
    }

    func presets() async throws -> [CCRProviderPreset] {
        let rows: [CoreCCRPreset] = try await provider.client().send(
            "ccr_call",
            body: CoreCCRCall(method: "getProviderPresets", args: [])
        )
        return rows.map(\.domain)
    }

    func accounts(provider name: String, forceRefresh: Bool) async throws -> [CCRAccount] {
        let rows: [CoreCCRAccount] = try await provider.client().send(
            "ccr_call",
            body: CoreCCRCall(
                method: "getProviderAccountSnapshots",
                args: [.string(name), .object(forceRefresh ? ["forceRefresh": true] : [:])]
            )
        )
        return rows.map(\.domain)
    }

    func saveProvider(_ draft: CCRProviderDraft) async throws -> CCRProvider {
        let saved: CoreCCRProvider = try await provider.client().send(
            "ccr_provider_save",
            body: CoreCCRProviderSave(draft)
        )
        return saved.domain(disabledModels: [])
    }

    func deleteProvider(id: String) async throws {
        try await provider.client().sendIgnoring(
            "ccr_provider_delete",
            body: CoreCCRIdBody(id: id)
        )
    }

    func setModelEnabled(providerID: String, model: String, enabled: Bool) async throws {
        try await provider.client().sendIgnoring(
            "ccr_provider_set_model",
            body: CoreCCRModelToggle(providerId: providerID, model: model, enabled: enabled)
        )
    }

    func setCredentialEnabled(
        providerID: String,
        credentialID: String,
        enabled: Bool
    ) async throws {
        try await provider.client().sendIgnoring(
            "ccr_provider_set_credential",
            body: CoreCCRCredentialToggle(
                providerId: providerID,
                credentialId: credentialID,
                enabled: enabled
            )
        )
    }

    func probeProvider(baseURL: String, apiKey: String?) async throws -> CCRProbeResult {
        let result: CoreCCRProbeResult = try await provider.client().send(
            "ccr_provider_probe",
            body: CoreCCRProbeBody(baseURL: baseURL, apiKey: apiKey)
        )
        return CCRProbeResult(
            models: result.models,
            baseURL: result.normalizedBaseUrl,
            protocolName: result.detectedProtocol
        )
    }
}

private extension CoreCCRProvider {
    func domain(disabledModels: [String]) -> CCRProvider {
        var resolved = (credentials ?? []).map(\.domain)
        // Older CCR configs keep the key at the provider level. Surface it as
        // one credential so the list, the masked display and the probe share a
        // single path.
        if resolved.isEmpty, let apiKey, !apiKey.isEmpty {
            resolved = [
                CCRCredential(
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

        return CCRProvider(
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

private extension CoreCCRCredential {
    var domain: CCRCredential {
        CCRCredential(
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

private extension CoreCCRAccount {
    var domain: CCRAccount {
        CCRAccount(
            id: credentialId ?? credentialLabel ?? UUID().uuidString,
            label: credentialLabel ?? credentialId ?? "Key",
            status: status,
            meters: meters.map(\.domain)
        )
    }
}

private extension CoreCCRMeter {
    var domain: CCRMeter {
        CCRMeter(
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
