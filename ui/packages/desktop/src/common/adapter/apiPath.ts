/**
 * Backend API version root — the single frontend seam for versioning.
 *
 * Every local `/api/*` request must carry the version segment. Call sites keep
 * the upstream-style unversioned paths (`/api/...`) and this helper adds
 * `/api/v1` once, in the transport layer, so no caller (ipcBridge included)
 * hardcodes the version. Backend truth source is `core/version.py:api_prefix()`.
 */

export const API_VERSION_ROOT = '/api/v1';

/**
 * Prefix a local API path with the version root.
 *
 * Leaves non-`/api` paths (static assets, `/ws`, absolute URLs) and already
 * versioned paths untouched, so the rewrite is idempotent.
 */
export function withApiVersion(path: string): string {
  if (path !== '/api' && !path.startsWith('/api/')) {
    return path;
  }
  if (path === API_VERSION_ROOT || path.startsWith(`${API_VERSION_ROOT}/`)) {
    return path;
  }
  return `${API_VERSION_ROOT}${path.slice('/api'.length)}`;
}
