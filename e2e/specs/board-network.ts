/** Browser board transport boundaries. Legacy /data remains an API inventory. */
export interface BoardOptions {
  limit?: number;
  column?: string;
  cursor?: string;
  drafts?: boolean;
  [key: string]: unknown;
}

export function boardOptions(url: URL): BoardOptions | null {
  if (!/^\/api\/repos\/[^/]+\/board$/.test(url.pathname)) return null;
  try {
    const options: unknown = JSON.parse(url.searchParams.get("options") || "null");
    return options && typeof options === "object" && !Array.isArray(options)
      ? options as BoardOptions : null;
  } catch {
    return null;
  }
}

function matchesProject(url: URL, slug?: string): boolean {
  return slug === undefined || url.pathname === `/api/repos/${encodeURIComponent(slug)}/board`;
}

export function isBoardMetadata(url: URL, slug?: string): boolean {
  return matchesProject(url, slug) && boardOptions(url)?.limit === 0;
}

/** Undefined column accepts any summary page; null selects the List page. */
export function isBoardPage(url: URL, slug?: string, column?: string | null): boolean {
  const options = boardOptions(url);
  return matchesProject(url, slug) && !!options && typeof options.limit === "number" &&
    options.limit > 0 && !options.drafts &&
    (column === undefined || (column === null ? options.column === undefined : options.column === column));
}

export function isCatalog(url: URL): boolean {
  return url.pathname === "/api/repos";
}
