// [crewchat] Remember models a provider knows about but the user switched off.
//
// CCR has no per-model "disabled" flag: a model is routable exactly while it is
// inside `provider.models`. So the settings UI keeps the switched-off ones in
// local storage, which is what lets the card keep showing them with their
// switch off (and re-enable them without a fresh detection).

const STORAGE_KEY = "crewchat.ccr.disabledModels.v1"

type DisabledModelMap = Record<string, string[]>

function readAll(): DisabledModelMap {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return {}
    const parsed: unknown = JSON.parse(raw)
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      Array.isArray(parsed)
    ) {
      return {}
    }
    const out: DisabledModelMap = {}
    for (const [key, value] of Object.entries(parsed)) {
      if (Array.isArray(value)) {
        out[key] = value.filter(
          (item): item is string => typeof item === "string"
        )
      }
    }
    return out
  } catch {
    return {}
  }
}

function writeAll(map: DisabledModelMap): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(map))
  } catch {
    // Storage unavailable (private mode): the switches just won't survive a reload.
  }
}

export function readDisabledModels(providerKey: string): string[] {
  return readAll()[providerKey] ?? []
}

export function writeDisabledModels(
  providerKey: string,
  models: string[]
): void {
  const map = readAll()
  const unique = [
    ...new Set(models.map((model) => model.trim()).filter(Boolean)),
  ]
  if (unique.length === 0) {
    delete map[providerKey]
  } else {
    map[providerKey] = unique
  }
  writeAll(map)
}
