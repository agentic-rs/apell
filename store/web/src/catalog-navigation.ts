interface CatalogEntry {
  catalog_query?: string;
  catalog_scroll_y: number;
}

export function catalogEntry(from: URL, destination: URL, scroll_y: number): CatalogEntry | null {
  if (
    from.origin !== destination.origin ||
    from.pathname !== '/' ||
    !/^\/applets\/[a-z0-9_-]+\/?$/.test(destination.pathname)
  ) {
    return null;
  }
  const query = from.searchParams.get('q');
  return {
    ...(query?.trim() ? { catalog_query: query } : {}),
    catalog_scroll_y: scroll_y,
  };
}

export function catalogReturn(state: unknown): { href: string; label: string; scroll_y: number } {
  const entry = state && typeof state === 'object' ? (state as Partial<CatalogEntry>) : undefined;
  const query = entry?.catalog_query;
  const saved = entry?.catalog_scroll_y;
  const scroll_y = typeof saved === 'number' && Number.isFinite(saved) && saved >= 0 ? saved : 0;
  if (typeof query !== 'string' || !query.trim()) {
    return { href: '/', label: '← All applets', scroll_y };
  }
  return { href: `/?${new URLSearchParams({ q: query })}`, label: '← Back to results', scroll_y };
}
