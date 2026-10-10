import { describe, expect, it } from 'vitest';
import { MANAGER_ID, buildGroupRoster, isGroupCrew, uniqueWorkers } from './groupRoster';

describe('groupRoster', () => {
  it('drops the manager, empties, and repeats', () => {
    expect(uniqueWorkers([MANAGER_ID, 'coder', 'coder', '', 'reviewer'])).toEqual(['coder', 'reviewer']);
  });

  it('puts the manager first on the wire', () => {
    expect(buildGroupRoster(['coder', MANAGER_ID, 'coder'])).toEqual([MANAGER_ID, 'coder']);
  });

  it('treats two members as a group and one as a 1:1', () => {
    expect(isGroupCrew(['coder'])).toBe(false);
    expect(isGroupCrew(['coder', 'reviewer'])).toBe(true);
  });
});
