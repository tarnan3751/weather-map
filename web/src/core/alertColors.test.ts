import { describe, expect, it } from 'vitest';
import { alertClickRank } from './alertColors.ts';

describe('which alert a click opens', () => {
  it('ranks by the tiles’ level, then warnings before watches', () => {
    // A ring of shapeless alerts: markers carry `level`, tile features too.
    const ring = [
      { id: 'a', event: 'Thunderstorm', severity: 'Minor', level: 1 },
      { id: 'b', event: 'Heavy rain', level: 3 },
      { id: 'c', event: 'Wind', severity: 'Moderate', level: 2 },
      { id: 'd', event: 'Flood Watch', severity: 'Severe', level: 3 },
      { id: 'e', event: 'Flood Warning', severity: 'Severe', level: 3 },
    ];
    expect([...ring].sort((x, y) => alertClickRank(y) - alertClickRank(x)).map((p) => p.id)).toEqual(['e', 'd', 'b', 'c', 'a']);
    // Without a level, the severity decides.
    expect(alertClickRank({ event: 'Gale', severity: 'Extreme' })).toBeGreaterThan(alertClickRank({ event: 'Gale Warning', severity: 'Severe' }));
  });
});
