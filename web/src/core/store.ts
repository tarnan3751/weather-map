import { useCallback, useRef, useSyncExternalStore } from 'react';

/** A minimal observable store: the map engine and React both subscribe. */
export class Store<S extends object> {
  private state: S;
  private listeners = new Set<(s: S, prev: S) => void>();

  constructor(initial: S) {
    this.state = initial;
  }

  get(): S {
    return this.state;
  }

  set(patch: Partial<S> | ((s: S) => Partial<S>)) {
    const prev = this.state;
    const p = typeof patch === 'function' ? patch(prev) : patch;
    let changed = false;
    for (const k in p) {
      if (!Object.is(prev[k], p[k])) {
        changed = true;
        break;
      }
    }
    if (!changed) return;
    this.state = { ...prev, ...p };
    for (const l of this.listeners) l(this.state, prev);
  }

  subscribe(fn: (s: S, prev: S) => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }
}

/** Subscribe a component to a slice of a store. Re-renders only when the
 *  selected value changes (by `Object.is`, or `equal` when given). */
export function useStore<S extends object, T>(store: Store<S>, select: (s: S) => T, equal: (a: T, b: T) => boolean = Object.is): T {
  const last = useRef<{ v: T } | null>(null);
  const getSnapshot = useCallback(() => {
    const v = select(store.get());
    if (last.current && equal(last.current.v, v)) return last.current.v;
    last.current = { v };
    return v;
  }, [store, select, equal]);
  const subscribe = useCallback((cb: () => void) => store.subscribe(cb), [store]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

export const shallowEqual = <T,>(a: T, b: T): boolean => {
  if (Object.is(a, b)) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || !a || !b) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => Object.is((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]));
};
