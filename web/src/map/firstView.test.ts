import { describe, expect, it } from 'vitest';
import { memoryStorage } from '../testing.ts';
import { guessView, savedView, saveView } from './firstView.ts';
import { zonePlace } from './zones.ts';

describe('the first view', () => {
  it('is where the user left the map', () => {
    const s = memoryStorage();
    expect(savedView(s)).toBeNull();
    saveView(-190.5, 50.25, 5.5, s);
    expect(savedView(s)).toEqual({ kind: 'center', center: [169.5, 50.25], zoom: 5.5 });
    s.setItem('wx.view', '{"lng":"x"}');
    expect(savedView(s)).toBeNull();
    s.setItem('wx.view', 'junk');
    expect(savedView(s)).toBeNull();
  });

  it('frames the mainland US for US time zones, as before', async () => {
    const v = await guessView('America/Chicago', 1440);
    expect(v.kind).toBe('bounds');
    const narrow = await guessView('America/Los_Angeles', 390);
    expect(narrow.kind).toBe('bounds');
  });

  it('centres on the time zone’s part of the world elsewhere', async () => {
    const berlin = await guessView('Europe/Berlin', 1440);
    expect(berlin).toEqual({ kind: 'center', center: [13.4, 52.5], zoom: 4 });
    const tokyo = await guessView('Asia/Tokyo', 390);
    expect(tokyo.kind === 'center' && tokyo.center[0] > 135 && tokyo.zoom < 4).toBe(true);
    // Alaska and Hawaii are not the mainland.
    expect((await guessView('America/Anchorage', 1440)).kind).toBe('center');
    expect((await guessView('Pacific/Honolulu', 1440)).kind).toBe('center');
    // Old names browsers still report.
    expect((await guessView('Asia/Calcutta', 1440)).kind).toBe('center');
    expect(zonePlace('Asia/Calcutta')).toEqual(zonePlace('Asia/Kolkata'));
  });

  it('shows the world when the zone names no place, or a band of it by offset', async () => {
    const utc = await guessView('UTC', 1440);
    expect(utc.kind === 'center' && utc.center[0] === 0 && utc.zoom > 1).toBe(true);
    const gmt3 = await guessView('Etc/GMT+3', 1440, 180);
    expect(gmt3.kind === 'center' && gmt3.center[0]).toBe(-45);
    expect((await guessView(undefined, 800)).kind).toBe('center');
  });
});
