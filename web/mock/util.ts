// Time units and number formatting shared by the mock's modules.
// Erasable syntax only: worker threads load this directly.

export const MIN = 60_000;
export const HOUR = 60 * MIN;
export const DAY = 24 * HOUR;

/** ISO 8601 to the second, as the server writes times. */
export const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z');
export const round = (v: number, digits: number) => Math.round(v * 10 ** digits) / 10 ** digits;
export const pad = (n: number, width = 2) => String(Math.round(n)).padStart(width, '0');
