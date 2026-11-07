import { createContext, useContext } from 'react';
import type { DataController } from '../app/data.ts';
import type { Playback } from '../app/playback.ts';
import type { AppState } from '../app/state.ts';
import type { Store } from '../core/store.ts';
import { useStore } from '../core/store.ts';
import type { MapController } from '../map/mapController.ts';

export interface AppContext {
  store: Store<AppState>;
  data: DataController;
  playback: Playback;
  map: () => MapController | null;
  reducedMotion: () => boolean;
}

export const Ctx = createContext<AppContext | null>(null);

export function useApp(): AppContext {
  const c = useContext(Ctx);
  if (!c) throw new Error('no app context');
  return c;
}

export function useAppState<T>(select: (s: AppState) => T, equal?: (a: T, b: T) => boolean): T {
  const { store } = useApp();
  return useStore(store, select, equal);
}
