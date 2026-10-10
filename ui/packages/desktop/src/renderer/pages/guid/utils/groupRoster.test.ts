import { describe, expect, it } from 'vitest';
import { MANAGER_ID, buildGroupRoster, uniqueWorkers } from './groupRoster';

describe('groupRoster', () => {
  it('drops the manager, empties, and repeats', () => {
    expect(uniqueWorkers([MANAGER_ID, 'coder', 'coder', '', 'reviewer'])).toEqual(['coder', 'reviewer']);
  });

  it('puts the manager first on the wire', () => {
    expect(buildGroupRoster(['coder', MANAGER_ID, 'coder'])).toEqual([MANAGER_ID, 'coder']);
  });
});
