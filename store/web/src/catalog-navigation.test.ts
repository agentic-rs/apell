import { describe, expect, it } from 'vitest';
import { catalogEntry, catalogReturn } from './catalog-navigation';

const catalog = 'https://store.example.invalid/';
const details = new URL('applets/paper-reader?channel=nightly', catalog);

describe('returning from an applet to the catalog', () => {
  it('preserves and encodes the active search without putting it in the applet link', () => {
    const from = new URL('?q=paper+%26+notes', catalog);
    expect(catalogReturn(catalogEntry(from, details))).toEqual({
      href: '/?q=paper+%26+notes',
      label: '← Back to results',
    });
    expect(details.searchParams.has('q')).toBe(false);
  });

  it('opens the full catalog for direct details or an empty search', () => {
    expect(catalogReturn(null)).toEqual({ href: '/', label: '← All applets' });
    expect(catalogReturn({ catalog_query: 17 }).href).toBe('/');
    expect(catalogReturn(catalogEntry(new URL('?q=+++', catalog), details)).href).toBe('/');
  });

  it('records search context only for local catalog-to-applet navigation', () => {
    const filtered = new URL('?q=reader', catalog);
    expect(catalogEntry(filtered, new URL('account', catalog))).toBeNull();
    expect(catalogEntry(details, details)).toBeNull();
    expect(
      catalogEntry(filtered, new URL('https://other.example.invalid/applets/paper-reader')),
    ).toBeNull();
  });
});
