/**
 * @license
 * Copyright 2025 AionUi (aionui.com)
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Wire-contract types for `/api/providers/*`.
 *
 * Mirrors the backend `core/providers.py` (`Model` / `Provider`) and the
 * cc-switch-backed enrichment in `core/model_catalog.py`.
 */

/** Fixed tag vocabulary. The backend validates against this same list. */
export type ModelTag = 'text' | 'vision' | 'reasoning' | 'tool_use' | 'long_context' | 'fast' | 'free' | 'embedding';

/** Per-million-token costs, sourced from the cc-switch models.dev catalog. */
export interface IModelPrice {
  display_name?: string;
  input?: number;
  output?: number;
  cache_read?: number;
  cache_write?: number;
}

/**
 * Model capabilities. `vision` is only present when cc-switch reached a
 * conclusion — an unknown capability is omitted (fail-open), never `false`.
 */
export interface IModelCapabilities {
  vision?: boolean;
}

/** One model under a provider. */
export interface IModelView {
  id: string;
  /** User-supplied display name; empty means "fall back to id". */
  display_name: string;
  /** Resolved display label: `display_name || id`. */
  label: string;
  /** 自动推导 ∪ 用户手工补充 */
  tags: ModelTag[];
  /** 其中由后端推导得出（前端用来区分展示，用户不能取消这些） */
  auto_tags?: ModelTag[];
  context_limit?: number | null;
  enabled: boolean;
  protocol?: string | null;
  capabilities: IModelCapabilities;
  price: IModelPrice;
}

/** One account key. The list endpoint never includes the secret. */
export interface IProviderKey {
  id: string;
  remark: string;
  is_default: boolean;
  has_secret: boolean;
}

/** Sent when saving. Omit `secret` to keep the stored value. */
export interface IProviderKeyInput {
  id: string;
  remark: string;
  is_default: boolean;
  secret?: string;
}

/** A provider (brain-pool entry). `api_key` is always masked on the wire. */
export interface IProvider {
  id: string;
  platform: string;
  name: string;
  base_url: string;
  /** Masked placeholder when a key is set; empty string when not. */
  api_key: string;
  has_key: boolean;
  /** Number of usable keys. */
  key_count: number;
  /** Accounts under this provider. Absent on older payloads. */
  keys?: IProviderKey[];
  model: string;
  models: IModelView[];
  model_count: number;
  api_format: string;
  temperature?: number | null;
  max_tokens?: number | null;
  extra_headers: Record<string, string>;
  enabled: boolean;
  is_full_url: boolean;
  models_url_override: string;
}

/** Model ids of a provider, for pickers and selectors that work with ids. */
export const modelIds = (provider?: IProvider | null): string[] => (provider?.models ?? []).map((m) => m.id);

/** First model id of a provider, or undefined when it has none. */
export const firstModelId = (provider?: IProvider | null): string | undefined => modelIds(provider)[0];

/**
 * Body for adding/updating one model.
 *
 * Tags are deliberately absent: they are always derived server-side from
 * cc-switch's capability registry, the models.dev price catalog and naming
 * conventions. Manual tags would only fight the derivation.
 */
export interface UpsertModelRequest {
  id: string;
  /** The only user-facing override, alongside `context_limit`. */
  display_name?: string;
  context_limit?: number | null;
  enabled?: boolean;
  protocol?: string | null;
}

/** Body for bulk enable/disable. `ids: null` applies to every model. */
export interface BulkToggleModelsRequest {
  provider_id: string;
  ids?: string[] | null;
  enabled: boolean;
}

/** A model discovered from the provider's own endpoint. */
export interface IDiscoveredModel {
  id: string;
  owned_by?: string;
  price?: IModelPrice;
}

export interface CreateProviderRequest {
  /** Optional caller-supplied id; the server generates one when omitted. */
  id?: string;
  name: string;
  base_url: string;
  api_key: string;
  keys?: IProviderKeyInput[];
  api_format?: string;
  enabled?: boolean;
  is_full_url?: boolean;
  models_url_override?: string;
  /** Full model objects, used by the legacy config migration. */
  models?: IModelView[];
}

/** Partial update for `PUT /api/providers/:id`; only sent fields are applied. */
export interface UpdateProviderRequest extends Partial<CreateProviderRequest> {}

/** Response for `POST /api/providers/fetch-models`. */
export interface FetchModelsResponse {
  models: IDiscoveredModel[];
  success: boolean;
  message?: string;
}

/** Anonymous fetch-models request used by the pre-create form flow. */
export interface FetchModelsAnonymousRequest {
  base_url?: string;
  api_key: string;
  api_format?: string;
  is_full_url?: boolean;
  extra_headers?: Record<string, string>;
  provider_id?: string;
}