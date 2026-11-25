export function accountReturnTo(
  location: Pick<Location, 'pathname' | 'search'>,
): string | undefined {
  const path = location.pathname.replace(/\/$/, '');
  if (path === '/account/access' || path === '/account/new') return path;
  if (!/^\/account\/packages\/[a-z0-9_-]{1,80}$/.test(path)) return undefined;

  const tabs = new URLSearchParams(location.search).getAll('tab');
  const tab = tabs.length === 1 ? tabs[0] : undefined;
  return tab === 'releases' || tab === 'publishing' || tab === 'people'
    ? `${path}?tab=${tab}`
    : path;
}
