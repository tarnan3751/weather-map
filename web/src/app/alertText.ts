// What the alert panel shows from an alert: which language block, the text
// taken from it (with the language each piece is really in, for `lang`), the
// names of the other languages, and links that are safe to open. Pure, so it
// can be tested without a browser.

import type { AlertDetail, AlertInfo, AlertProps } from '../api/types.ts';

/** A piece of alert text and the language it is written in. */
export interface LangText {
  text: string;
  lang: string;
}

/** A list entry or a full detail: the panel shows the list's summary while
 *  the detail loads. */
type AlertLike = AlertProps & Partial<Pick<AlertDetail, 'description' | 'instruction' | 'sender_name' | 'infos'>>;

interface AlertText {
  /** Index into `infos` of the block shown; -1 when there are none. */
  index: number;
  event: LangText;
  headline: LangText | null;
  description: LangText | null;
  instruction: LangText | null;
  area: LangText | null;
  sender: LangText | null;
  /** An http(s) link, or null. */
  web: string | null;
}

const primaryTag = (tag: string) => tag.split(/[-_]/)[0].toLowerCase();
const sameTag = (a: string, b: string) => a.replace(/_/g, '-').toLowerCase() === b.replace(/_/g, '-').toLowerCase();

/** The browser's preferred languages, best first. */
export function browserLanguages(): readonly string[] {
  if (typeof navigator === 'undefined') return [];
  if (navigator.languages?.length) return navigator.languages;
  return navigator.language ? [navigator.language] : [];
}

/** The language the interface is written in. */
export function uiLanguage(): string {
  return (typeof document !== 'undefined' && document.documentElement.lang) || 'en';
}

/** The block in the alert's own shown language, else the first. */
export function primaryIndex(infos: readonly AlertInfo[], language: string): number {
  const i = infos.findIndex((b) => sameTag(b.language, language));
  return i >= 0 ? i : 0;
}

/**
 * The block to show: for each preferred language in turn, a block with
 * exactly that tag, else one with the same language part (`de-CH` and `de`),
 * so a reader who puts French before English gets French even when English
 * is there too. Failing all, the primary block; -1 when there are no blocks.
 */
export function chooseInfo(infos: readonly AlertInfo[], language: string, prefs: readonly string[]): number {
  if (infos.length === 0) return -1;
  // Blocks in effect first: an earlier step of the same message in the same
  // language is shown only when picked.
  const order = [...infos.keys()].sort((i, j) => Number(!inEffect(infos[i])) - Number(!inEffect(infos[j])));
  for (const p of prefs) {
    if (!p) continue;
    const exact = order.find((i) => sameTag(infos[i].language, p));
    if (exact !== undefined) return exact;
    const near = order.find((i) => primaryTag(infos[i].language) === primaryTag(p));
    if (near !== undefined) return near;
  }
  return order.find((i) => sameTag(infos[i].language, language)) ?? order[0];
}

/** A block counts unless the server says it does not. */
export const inEffect = (b: Pick<AlertInfo, 'in_effect'>) => b.in_effect !== false;

/** A link only when it is an absolute http(s) URL. */
export function safeWebLink(url: string | null | undefined): string | null {
  const u = url?.trim();
  if (!u || !/^https?:\/\//i.test(u)) return null;
  try {
    const parsed = new URL(u);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? parsed.href : null;
  } catch {
    return null;
  }
}

function text(value: string | null | undefined, lang: string): LangText | null {
  const t = value?.trim();
  return t ? { text: t, lang } : null;
}

/**
 * The text to show, from the chosen block, each field falling back to the
 * alert's own top-level field (written in the alert's `language`) when the
 * block lacks it, and the link to the primary block's.
 */
export function alertText(a: AlertLike, prefs: readonly string[], picked?: number | null): AlertText {
  const infos = a.infos ?? [];
  const index = picked !== undefined && picked !== null && picked >= 0 && picked < infos.length ? picked : chooseInfo(infos, a.language, prefs);
  const b = index >= 0 ? infos[index] : null;
  const own = a.language;
  // The top-level fields are the blocks in effect: a block that is not does
  // not borrow their words (another step's instruction is not its own).
  const borrow = !b || inEffect(b);
  const pick = (blockValue: string | null | undefined, top: string | null | undefined) =>
    (b && text(blockValue, b.language)) || (borrow ? text(top, own) : null);
  const primary = infos.length > 0 ? infos[primaryIndex(infos, own)] : null;
  return {
    index,
    event: pick(b?.event, a.event) ?? { text: a.event, lang: own },
    headline: pick(b?.headline, a.headline),
    description: pick(b?.description, a.description),
    instruction: pick(b?.instruction, a.instruction),
    area: pick(b?.area_desc, a.area_desc),
    sender: pick(b?.sender_name, a.sender_name),
    web: safeWebLink(b?.web) ?? safeWebLink(primary?.web),
  };
}

function displayName(tag: string, ui: string): string {
  try {
    return new Intl.DisplayNames([ui], { type: 'language' }).of(tag) ?? tag;
  } catch {
    return tag;
  }
}

/**
 * Names for the language switcher, in the interface's language: just the
 * language ("French") unless two blocks share it, then the full name
 * ("Canadian French"), and the tag itself when even that is ambiguous.
 */
export function languageLabels(tags: readonly string[], ui: string): string[] {
  const bases = tags.map(primaryTag);
  const labels = tags.map((t, i) => (bases.some((b, j) => j !== i && b === bases[i]) ? displayName(t, ui) : displayName(bases[i], ui)));
  return labels.map((l, i) => (labels.some((m, j) => j !== i && m === l) ? `${l} (${tags[i]})` : l));
}

/** When an alert ends: NWS `ends`, else `expires`. `about` when the sender
 *  gave no end and the server assumed one. */
export function alertEnd(a: Pick<AlertProps, 'ends' | 'expires' | 'expires_assumed'>): { at: number; about: boolean } | null {
  const iso = a.ends ?? a.expires;
  const at = iso ? Date.parse(iso) : Number.NaN;
  if (!Number.isFinite(at)) return null;
  return { at, about: !a.ends && !!a.expires_assumed };
}

/** Who issued it: the register's source, with the country when the source's
 *  name does not already say it. */
export function issuer(d: Pick<AlertDetail, 'source_name' | 'country_name'>): string | null {
  const s = d.source_name?.trim();
  const c = d.country_name?.trim();
  if (s && c && !s.includes(c)) return `${s} (${c})`;
  return s || c || null;
}

/** The host a link goes to, to name it by. */
export function linkHost(url: string): string {
  try {
    return new URL(url).host.replace(/^www\./, '');
  } catch {
    return url;
  }
}
