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
        return config.providers.map(\.domain)
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
        return saved.domain
    }

    func deleteProvider(id: String) async throws {
        try await provider.client().sendIgnoring(
            "ccr_provider_delete",
            body: CoreCCRIdBody(id: id)
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
    var domain: CCRProvider {
        var resolved = (credentials ?? []).map(\.domain)
        // Older CCR configs keep the key at the provider level. Surface it as
        // one credential so the list, the masked display and the probe share a
        // single path.
        if resolved.isEmpty, let apiKey, !apiKey.isEmpty {
            resolved = [
                CCRCredential(id: "api_key", name: "Key", apiKey: apiKey, enabled: true)
            ]
        }

        return CCRProvider(
            id: id ?? name,
            name: name,
            type: type ?? "—",
            baseURL: baseURL ?? "—",
            enabled: enabled ?? true,
            models: models ?? [],
            credentials: resolved
        )
    }
}

private extension CoreCCRCredential {
    var domain: CCRCredential {
        CCRCredential(
            id: id ?? name ?? label ?? UUID().uuidString,
            name: name ?? label ?? "Key",
            apiKey: apiKey ?? "",
            enabled: enabled ?? true
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
