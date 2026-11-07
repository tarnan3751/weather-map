import { describe, expect, it } from 'vitest';
import type { AlertDetail, AlertInfo } from '../api/types.ts';
import { alertEnd, alertText, chooseInfo, issuer, languageLabels, linkHost, primaryIndex, safeWebLink } from './alertText.ts';

const info = (language: string, over: Partial<AlertInfo> = {}): AlertInfo => ({
  language,
  event: `event ${language}`,
  headline: `headline ${language}`,
  description: `description ${language}`,
  instruction: `instruction ${language}`,
  area_desc: `area ${language}`,
  sender_name: `sender ${language}`,
  web: `https://example.org/${language}`,
  category: 'met',
  severity: 'Severe',
  urgency: 'Expected',
  certainty: 'Likely',
  onset: null,
  expires: null,
  ...over,
});

const swiss = [info('de-CH'), info('fr-CH'), info('it-CH', { instruction: null, web: null }), info('en-GB')];

function detail(infos: AlertInfo[], language: string): AlertDetail {
  const shown = infos[primaryIndex(infos, language)];
  return {
    id: 'cap:00000000000000000000',
    event: shown.event,
    severity: 'Severe',
    urgency: 'Expected',
    certainty: 'Likely',
    headline: shown.headline,
    area_desc: shown.area_desc,
    sent: null,
    onset: null,
    ends: null,
    expires: null,
    geom_source: 'cap',
    source: 'ch-meteoswiss-en',
    country: 'ch',
    language,
    category: 'met',
    description: shown.description,
    instruction: shown.instruction,
    sender_name: shown.sender_name,
    message_type: 'Alert',
    status: 'Actual',
    active: true,
    source_name: 'Switzerland: MeteoSwiss',
    country_name: 'Switzerland',
    infos,
  };
}

describe('blocks no longer in effect', () => {
  // Two English steps of one message, the first one over.
  const steps = [info('en-AU', { headline: 'eased', in_effect: false }), info('en-AU', { headline: 'rising' }), info('fr-FR', { in_effect: false })];

  it('are passed over for a block in effect, unless picked', () => {
    expect(chooseInfo(steps, 'en-AU', ['en-AU'])).toBe(1);
    expect(chooseInfo(steps, 'en-AU', ['fr'])).toBe(2);
    expect(alertText(detail(steps, 'en-AU'), ['en']).headline?.text).toBe('rising');
    expect(alertText(detail(steps, 'en-AU'), ['en'], 0).headline?.text).toBe('eased');
    // A block over does not borrow the live block's instruction.
    const quiet = [info('en-AU', { instruction: null, in_effect: false }), info('fr-FR')];
    expect(alertText({ ...detail(quiet, 'fr-FR'), instruction: 'live instruction' }, ['en'], 0).instruction).toBeNull();
    expect(alertText({ ...detail(quiet, 'fr-FR'), instruction: 'live instruction' }, ['en'], 1).instruction?.text).toBe('instruction fr-FR');
    // An older server says nothing: every block counts.
    expect(chooseInfo([info('en-AU', { headline: 'a' }), info('en-AU')], 'en-AU', ['en'])).toBe(0);
  });
});

describe('choosing the language block', () => {
  it('takes the exact tag first, then the language part, per preference', () => {
    expect(chooseInfo(swiss, 'en-GB', ['fr-CH', 'en'])).toBe(1);
    expect(chooseInfo(swiss, 'en-GB', ['de-AT', 'en-GB'])).toBe(0);
    expect(chooseInfo(swiss, 'en-GB', ['it'])).toBe(2);
    expect(chooseInfo([info('de'), info('fr')], 'de', ['fr-CH'])).toBe(1);
    expect(chooseInfo([info('en-US'), info('en-GB')], 'en-US', ['en-GB'])).toBe(1);
    expect(chooseInfo(swiss, 'en-GB', ['EN-gb'])).toBe(3);
  });

  it('falls back to the alert\'s own language, else the first block', () => {
    expect(chooseInfo(swiss, 'en-GB', ['ja-JP', 'ko'])).toBe(3);
    expect(chooseInfo(swiss, 'en-GB', [])).toBe(3);
    expect(chooseInfo(swiss, 'rm-CH', [])).toBe(0);
    expect(chooseInfo([], 'en', ['en'])).toBe(-1);
  });
});

describe('the text shown', () => {
  it('comes from the chosen block, tagged with its language', () => {
    const t = alertText(detail(swiss, 'en-GB'), ['fr-FR']);
    expect(t.index).toBe(1);
    expect(t.event).toEqual({ text: 'event fr-CH', lang: 'fr-CH' });
    expect(t.instruction).toEqual({ text: 'instruction fr-CH', lang: 'fr-CH' });
    expect(t.web).toBe('https://example.org/fr-CH');
  });

  it('falls back to the top-level fields, in the alert\'s language', () => {
    const t = alertText(detail(swiss, 'en-GB'), ['it-CH']);
    expect(t.event.lang).toBe('it-CH');
    expect(t.instruction).toEqual({ text: 'instruction en-GB', lang: 'en-GB' });
    expect(t.web).toBe('https://example.org/en-GB');
  });

  it('leaves out what is missing or blank', () => {
    const bare = info('pt-BR', { headline: null, instruction: '  ', sender_name: null, web: null });
    const d = { ...detail([bare], 'pt-BR'), headline: null, instruction: null, sender_name: null };
    const t = alertText(d, ['en-US']);
    expect([t.headline, t.instruction, t.sender, t.web]).toEqual([null, null, null, null]);
    expect(t.description?.text).toBe('description pt-BR');
  });

  it('works from a list summary before the detail arrives', () => {
    const { infos: _infos, description: _d, instruction: _i, sender_name: _s, ...summary } = detail(swiss, 'en-GB');
    const t = alertText(summary, ['fr']);
    expect(t.index).toBe(-1);
    expect(t.event).toEqual({ text: 'event en-GB', lang: 'en-GB' });
    expect(t.description).toBeNull();
  });
});

describe('when it ends', () => {
  it('prefers NWS ends, and says "about" only for an assumed expiry', () => {
    const t = '2026-11-07T00:00:00Z';
    expect(alertEnd({ ends: t, expires: '2026-11-06T18:00:00Z', expires_assumed: false })).toEqual({ at: Date.parse(t), about: false });
    expect(alertEnd({ ends: null, expires: t, expires_assumed: true })).toEqual({ at: Date.parse(t), about: true });
    expect(alertEnd({ ends: null, expires: t })).toEqual({ at: Date.parse(t), about: false });
    expect(alertEnd({ ends: null, expires: null })).toBeNull();
  });
});

describe('links and names', () => {
  it('opens only http(s) links', () => {
    expect(safeWebLink('https://weather.gc.ca/')).toBe('https://weather.gc.ca/');
    expect(safeWebLink(' http://example.org/a b ')).toBe('http://example.org/a%20b');
    expect(safeWebLink('javascript:alert(1)')).toBeNull();
    expect(safeWebLink('data:text/html,hi')).toBeNull();
    expect(safeWebLink('//example.org')).toBeNull();
    expect(safeWebLink('www.example.org')).toBeNull();
    expect(safeWebLink('https://')).toBeNull();
    expect(safeWebLink(null)).toBeNull();
    expect(linkHost('https://www.dwd.de/x')).toBe('dwd.de');
  });

  it('names languages plainly, and fully only when two share a language', () => {
    expect(languageLabels(['de-CH', 'fr-CH', 'it-CH', 'en-GB'], 'en')).toEqual(['German', 'French', 'Italian', 'English']);
    expect(languageLabels(['en-CA', 'fr-CA'], 'en')).toEqual(['English', 'French']);
    expect(languageLabels(['en-US', 'en-GB'], 'en')).toEqual(['American English', 'British English']);
    expect(languageLabels(['ar-AE', 'en-GB'], 'en')[0]).toBe('Arabic');
    expect(languageLabels(['en', 'en'], 'en')).toEqual(['English (en)', 'English (en)']);
    expect(languageLabels(['not a tag!'], 'en')).toEqual(['not a tag!']);
  });

  it('names the issuer once', () => {
    expect(issuer({ source_name: 'Canada: Meteorological Service of Canada', country_name: 'Canada' })).toBe('Canada: Meteorological Service of Canada');
    expect(issuer({ source_name: 'MetService', country_name: 'New Zealand' })).toBe('MetService (New Zealand)');
    expect(issuer({ source_name: null, country_name: 'Italy' })).toBe('Italy');
    expect(issuer({ source_name: null, country_name: null })).toBeNull();
  });
});
