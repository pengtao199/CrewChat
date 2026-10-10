export const MANAGER_ID = 'crewchat-manager';

export function uniqueWorkers(ids: string[]): string[] {
  const seen = new Set<string>();
  const workers: string[] = [];
  for (const id of ids) {
    if (!id || id === MANAGER_ID || seen.has(id)) continue;
    seen.add(id);
    workers.push(id);
  }
  return workers;
}

export function buildGroupRoster(workerIds: string[]): string[] {
  return [MANAGER_ID, ...uniqueWorkers(workerIds)];
}
