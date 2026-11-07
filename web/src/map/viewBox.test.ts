import { describe, expect, it } from 'vitest';
import { viewBox } from './viewBox.ts';

describe('the view box', () => {
  it('is the plain box for an ordinary view', () => {
    expect(viewBox(-110, 30, -80, 45)).toEqual({ west: -110, south: 30, east: -80, north: 45 });
  });

  it('wraps a view panned across the antimeridian, west then east of east', () => {
    // MapLibre reports a view centred on the date line as 170..190.
    expect(viewBox(170, 50, 190, 65)).toEqual({ west: 170, south: 50, east: -170, north: 65 });
    expect(viewBox(-190, 50, -170, 65)).toEqual({ west: 170, south: 50, east: -170, north: 65 });
    // A view a whole world to the east is the same place.
    expect(viewBox(250, 30, 280, 45)).toEqual({ west: -110, south: 30, east: -80, north: 45 });
  });

  it('is the whole world once the view is that wide', () => {
    expect(viewBox(-250, -80, 200, 80)).toEqual({ west: -180, south: -80, east: 180, north: 80 });
  });

  it('keeps a view that ends exactly on 180', () => {
    expect(viewBox(150, 0, 180, 20)).toEqual({ west: 150, south: 0, east: 180, north: 20 });
  });
});
