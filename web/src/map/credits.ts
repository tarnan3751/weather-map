// The map's credits: the basemap's (from the style's sources, as MapLibre's
// own attribution control does) and, next to it, every weather source with a
// part in view, which changes as the user pans and as frames change. Built on
// MapLibre's attribution markup and styles, including the compact button on
// narrow maps.

import type { IControl, Map as MlMap } from 'maplibre-gl';

export interface Credit {
  text: string;
  url?: string;
}

/** Wider than this, the credits are always spelled out. */
const COMPACT_BELOW_PX = 640;

export class CreditsControl implements IControl {
  private map: MlMap | null = null;
  private container: HTMLDetailsElement | null = null;
  private inner: HTMLElement | null = null;
  private weather: Credit[] = [];
  private extra: Credit[] = [];
  private html = '';

  onAdd(map: MlMap): HTMLElement {
    this.map = map;
    const c = document.createElement('details');
    c.className = 'maplibregl-ctrl maplibregl-ctrl-attrib';
    const button = document.createElement('summary');
    button.className = 'maplibregl-ctrl-attrib-button';
    button.title = button.ariaLabel = 'Data credits';
    button.addEventListener('click', this.toggle);
    const inner = document.createElement('div');
    inner.className = 'maplibregl-ctrl-attrib-inner';
    c.append(button, inner);
    this.container = c;
    this.inner = inner;
    map.on('styledata', this.update);
    map.on('resize', this.updateCompact);
    map.on('drag', this.collapse);
    this.updateCompact();
    this.update();
    return c;
  }

  onRemove() {
    this.map?.off('styledata', this.update);
    this.map?.off('resize', this.updateCompact);
    this.map?.off('drag', this.collapse);
    this.container?.remove();
    this.map = this.container = this.inner = null;
    this.html = '';
  }

  getDefaultPosition() {
    return 'bottom-right' as const;
  }

  /** The weather sources in view, top first. */
  setWeather(credits: Credit[]) {
    this.weather = credits;
    this.update();
  }

  /** Overlays' credits (alerts), after the weather's. */
  setExtra(credits: Credit[]) {
    this.extra = credits;
    this.update();
  }

  /** Plain text of what is shown, for tests. */
  get text(): string {
    return this.inner?.textContent ?? '';
  }

  private basemap(): string[] {
    const sources = this.map?.getStyle()?.sources ?? {};
    const out: string[] = [];
    for (const s of Object.values(sources)) {
      const a = (s as { attribution?: string }).attribution;
      if (a && !out.includes(a)) out.push(a);
    }
    return out;
  }

  private update = () => {
    if (!this.inner || !this.map) return;
    const link = (c: Credit) => {
      const t = escapeHtml(c.text);
      return c.url ? `<a href="${escapeHtml(c.url)}" target="_blank" rel="noopener">${t}</a>` : t;
    };
    const parts = this.basemap();
    if (this.weather.length) parts.push(`<span class="credits-weather">Weather: ${this.weather.map(link).join(', ')}</span>`);
    for (const c of this.extra) parts.push(link(c));
    const html = parts.join(' | ');
    if (html === this.html) return;
    this.html = html;
    // The basemap's credit comes from the style as markup (MapLibre shows it
    // the same way); everything added here is escaped.
    this.inner.innerHTML = html;
  };

  private updateCompact = () => {
    const c = this.container;
    const map = this.map;
    if (!c || !map) return;
    const compact = map.getCanvasContainer().offsetWidth <= COMPACT_BELOW_PX;
    c.classList.toggle('maplibregl-compact', compact);
    if (compact) c.removeAttribute('open');
    else c.setAttribute('open', '');
  };

  private toggle = () => {
    const c = this.container;
    if (!c?.classList.contains('maplibregl-compact')) return;
    if (c.classList.contains('maplibregl-compact-show')) {
      c.setAttribute('open', '');
      c.classList.remove('maplibregl-compact-show');
    } else {
      c.classList.add('maplibregl-compact-show');
      c.removeAttribute('open');
    }
  };

  private collapse = () => {
    const c = this.container;
    if (c?.classList.contains('maplibregl-compact')) {
      c.classList.remove('maplibregl-compact-show');
      c.removeAttribute('open');
    }
  };
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (ch) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[ch]!);
}
