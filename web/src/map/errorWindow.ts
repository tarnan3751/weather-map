/**
 * Decides when a run of errors means a source is down: `limit` errors within
 * `windowMs`, with no success in between. Scattered failures over a long
 * session (a dropped tile here and there) never add up to a switch.
 */
export class ErrorWindow {
  private times: number[] = [];
  private readonly limit: number;
  private readonly windowMs: number;

  constructor(limit: number, windowMs: number) {
    this.limit = limit;
    this.windowMs = windowMs;
  }

  /** Record a failure; true when the source should be given up on. */
  fail(now: number): boolean {
    this.times = this.times.filter((t) => now - t < this.windowMs);
    this.times.push(now);
    return this.times.length >= this.limit;
  }

  succeed() {
    this.times = [];
  }
}
