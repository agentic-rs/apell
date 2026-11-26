import { describe, expect, it } from 'vitest';
import { catalogEntry, catalogReturn } from './catalog-navigation';

const catalog = 'https://store.example.invalid/';
const details = new URL('applets/paper-reader?channel=nightly', catalog);

describe('returning from an applet to the catalog', () => {
  it('preserves and encodes the active search without putting it in the applet link', () => {
    const from = new URL('?q=paper+%26+notes', catalog);
    expect(catalogReturn(catalogEntry(from, details, 630))).toEqual({
      href: '/?q=paper+%26+notes',
      label: '← Back to results',
      scroll_y: 630,
    });
    expect(details.searchParams.has('q')).toBe(false);
  });

  it('opens the full catalog for direct details or an empty search', () => {
    expect(catalogReturn(null)).toEqual({ href: '/', label: '← All applets', scroll_y: 0 });
    expect(catalogReturn({ catalog_query: 17 }).href).toBe('/');
    expect(catalogReturn(catalogEntry(new URL('?q=+++', catalog), details, 620))).toEqual({
      href: '/',
      label: '← All applets',
      scroll_y: 620,
    });
  });

  it('ignores invalid scroll offsets without dropping the saved search', () => {
    for (const catalog_scroll_y of ['10', -1, Infinity, NaN]) {
      expect(catalogReturn({ catalog_query: 'reader', catalog_scroll_y })).toEqual({
        href: '/?q=reader',
        label: '← Back to results',
        scroll_y: 0,
      });
    }
  });

  it('records search context only for local catalog-to-applet navigation', () => {
    const filtered = new URL('?q=reader', catalog);
    expect(catalogEntry(filtered, new URL('account', catalog), 400)).toBeNull();
    expect(catalogEntry(details, details, 400)).toBeNull();
    expect(
      catalogEntry(filtered, new URL('https://other.example.invalid/applets/paper-reader'), 400),
    ).toBeNull();
  });
});
