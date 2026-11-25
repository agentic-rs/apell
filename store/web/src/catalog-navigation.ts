interface CatalogEntry {
  catalog_query: string;
}

export function catalogEntry(from: URL, destination: URL): CatalogEntry | null {
  if (
    from.origin !== destination.origin ||
    from.pathname !== '/' ||
    !/^\/applets\/[a-z0-9_-]+\/?$/.test(destination.pathname)
  ) {
    return null;
  }
  const query = from.searchParams.get('q');
  return query?.trim() ? { catalog_query: query } : null;
}

export function catalogReturn(state: unknown): { href: string; label: string } {
  const query =
    state && typeof state === 'object' && 'catalog_query' in state
      ? state.catalog_query
      : undefined;
  if (typeof query !== 'string' || !query.trim()) {
    return { href: '/', label: '← All applets' };
  }
  return { href: `/?${new URLSearchParams({ q: query })}`, label: '← Back to results' };
}
