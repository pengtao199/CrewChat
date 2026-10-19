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
}

private extension CoreCCRProvider {
    var domain: CCRProvider {
        CCRProvider(
            id: id ?? name,
            name: name,
            type: type ?? "—",
            baseURL: baseURL ?? "—",
            enabled: enabled ?? true,
            models: models ?? [],
            credentials: (credentials ?? []).map(\.domain)
        )
    }
}

private extension CoreCCRCredential {
    var domain: CCRCredential {
        CCRCredential(
            id: id ?? name ?? label ?? UUID().uuidString,
            name: name ?? label ?? "Key",
            apiKeyMasked: Self.mask(apiKey),
            enabled: enabled ?? true
        )
    }

    static func mask(_ key: String?) -> String {
        guard let key, !key.isEmpty else { return "—" }
        let prefix = key.prefix(6)
        return "\(prefix)…"
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
