import { afterEach, describe, expect, it, vi } from 'vitest';
import { stubStorage } from '../testing.ts';
import { createAppStore, savePrefs, windMarksShown } from './state.ts';

afterEach(() => vi.unstubAllGlobals());

describe('wind marks', () => {
  it('default to particles, and are remembered', () => {
    const storage = stubStorage();
    const store = createAppStore();
    expect(store.get().windMarks).toBe('particles');
    store.set({ windMarks: 'off' });
    savePrefs(store.get());
    expect(JSON.parse(storage.getItem('wx.prefs')!).wind).toBe('off');
    expect(createAppStore().get().windMarks).toBe('off');
  });

  it('keep an older "arrows off" setting', () => {
    stubStorage({ 'wx.prefs': JSON.stringify({ arrows: false }) });
    expect(createAppStore().get().windMarks).toBe('off');
    stubStorage({ 'wx.prefs': JSON.stringify({ arrows: true }) });
    expect(createAppStore().get().windMarks).toBe('particles');
  });

  it('show arrows instead of particles with reduced motion or no support', () => {
    const s = { windMarks: 'particles' as const, particlesOk: true, reducedMotion: false };
    expect(windMarksShown(s)).toBe('particles');
    expect(windMarksShown({ ...s, particlesOk: null })).toBe('particles');
    expect(windMarksShown({ ...s, reducedMotion: true })).toBe('arrows');
    expect(windMarksShown({ ...s, particlesOk: false })).toBe('arrows');
    expect(windMarksShown({ ...s, windMarks: 'off', reducedMotion: true })).toBe('off');
  });
});
