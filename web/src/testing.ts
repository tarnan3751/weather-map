// Fixtures shared by the unit tests.

import { vi } from 'vitest';
import type { LonLatBox, Part } from './api/types.ts';
import type { TFrame } from './core/timeline.ts';

/** 12:00 UTC on 6 November 2026, when the tests take place. */
export const T0 = Date.UTC(2026, 10, 6, 12, 0);

export const box = (west: number, south: number, east: number, north: number): LonLatBox => ({ west, south, east, north });
export const WORLD = box(-180, -90, 180, 90);

/** A part valid at T0, from the source its id starts with (`mrms-1`); observed unless it has a run. */
export const part = (id: string, bounds: LonLatBox, solid: LonLatBox | null, max_zoom: number, run: string | null = null): Part => ({
  id,
  source: id.split('-')[0],
  valid: '2026-11-06T12:00:00Z',
  run,
  kind: run ? 'forecast' : 'observed',
  bounds,
  solid,
  max_zoom,
});

/** An observed frame at `t`, with no parts. */
export const tframe = (t: number): TFrame => ({ valid: new Date(t).toISOString(), kind: 'observed', parts: [], t, stack: [] });

/** A Storage kept in a Map. */
export function memoryStorage(initial: Record<string, string> = {}) {
  const items = new Map(Object.entries(initial));
  return {
    getItem: (k: string) => items.get(k) ?? null,
    setItem: (k: string, v: string) => void items.set(k, v),
    removeItem: (k: string) => void items.delete(k),
  };
}

/** `localStorage` replaced by a memory one, until `vi.unstubAllGlobals()`. */
export function stubStorage(initial: Record<string, string> = {}) {
  const storage = memoryStorage(initial);
  vi.stubGlobal('localStorage', storage);
  return storage;
}
