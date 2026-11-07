import { describe, expect, it } from 'vitest';
import { ErrorWindow } from './errorWindow.ts';

describe('basemap error window', () => {
  it('gives up after a burst of failures', () => {
    const w = new ErrorWindow(4, 20_000);
    expect(w.fail(0)).toBe(false);
    expect(w.fail(100)).toBe(false);
    expect(w.fail(200)).toBe(false);
    expect(w.fail(300)).toBe(true);
  });

  it('ignores failures scattered over a long session', () => {
    const w = new ErrorWindow(4, 20_000);
    for (let i = 0; i < 50; i++) expect(w.fail(i * 30_000)).toBe(false);
  });

  it('starts over after a success', () => {
    const w = new ErrorWindow(4, 20_000);
    w.fail(0);
    w.fail(1);
    w.fail(2);
    w.succeed();
    expect(w.fail(3)).toBe(false);
  });
});
