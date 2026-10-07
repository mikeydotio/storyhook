import type { Page } from "@playwright/test";
import { boardOptions } from "./specs/board-network";

/** Synthetic rendering fixtures start from an independent, real test-project
 * snapshot. The legacy fixture read is test setup only; the browser receives
 * metadata, bounded summaries and explicit detail responses on separate routes.
 * This adapter never sorts: specs asserting order must provide the ordered
 * transport fixture and assert the requested sort; Rust owns comparator proof. */
export async function installBoardFixture(
  page: Page,
  slug: string,
  transform: (data: Record<string, any>, options: Record<string, any>) => void | Promise<void>,
): Promise<void> {
  const base = `/api/repos/${encodeURIComponent(slug)}`;
  const response = await page.request.get(new URL(base + "/data", page.url()).href, {
    headers: { "X-Storyhook": "1" },
  });
  if (!response.ok()) throw new Error(`board fixture snapshot failed: ${response.status()}`);
  const snapshot = await response.json();
  if (!snapshot.stories?.length) throw new Error("board fixture requires an independent real story template");
  const originalIds = new Set(snapshot.stories.map((view: any) => view.story.id));
  const fresh = async (options: Record<string, any>) => {
    const data = JSON.parse(JSON.stringify(snapshot));
    await transform(data, options);
    return data;
  };
  await page.route(url => url.pathname === base + "/board", async route => {
    const options = boardOptions(new URL(route.request().url())) as Record<string, any> | null;
    if (!options) throw new Error("board fixture request lacks valid options");
    const data = await fresh(options);
    const all = (data.stories || []).filter((view: any) => !!view.story.draft === !!options.drafts);
    const matches = all.filter((view: any) => {
      const story = view.story;
      const shown = view.display_state || story.state;
      return (options.show_archived || !story.hidden_at)
        && (options.states == null || options.states.includes(shown))
        && (options.types == null || options.types.includes(story.story_type))
        && (options.priorities == null || options.priorities.includes(story.priority))
        && (!options.text || `${story.id} ${story.title} ${(story.labels || []).join(" ")}`.toLowerCase().includes(options.text.toLowerCase()));
    });
    const columns: Record<string, number> = {};
    matches.forEach((view: any) => { const key = view.display_state || view.story.state; columns[key] = (columns[key] || 0) + 1; });
    const inPage = matches.filter((view: any) => {
      const shown = view.display_state || view.story.state;
      return (!options.column || shown === options.column) && !(options.hidden_columns || []).includes(shown);
    });
    const rows = options.limit === 0 ? [] : inPage.slice(0, options.limit || 50);
    data.stories = rows.map((view: any) => {
      const summary = JSON.parse(JSON.stringify(view));
      for (const key of ["description", "comments", "attachments", "referenced_by_commits"]) delete summary.story[key];
      summary.continuation_needs_attention = !!summary.continuation_alerts?.length;
      for (const key of ["derived_relationships", "referenced_by", "continuation_alerts"]) delete summary[key];
      summary.is_summary = true;
      const rank = (data.next_ids || []).indexOf(summary.story.id);
      summary.next_rank = rank < 0 ? null : rank;
      return summary;
    });
    const ids = new Set(rows.map((view: any) => view.story.id));
    data.ready_ids = rows.filter((view: any) => view.is_ready).map((view: any) => view.story.id);
    data.blocked_ids = rows.filter((view: any) => view.is_blocked).map((view: any) => view.story.id);
    data.next_ids = (data.next_ids || []).filter((id: string) => ids.has(id));
    data.drafts = [];
    data.counts = { all: all.length, total: matches.length, columns, drafts: 0 };
    data.page = { total: inPage.length, offset: 0, limit: options.limit, next_cursor: null, revision: "synthetic-render-fixture" };
    data.refs = {};
    await route.fulfill({ status: 200, contentType: "application/json", json: data });
  });
  await page.route(url => url.pathname.startsWith(base + "/story/") && url.pathname.slice((base + "/story/").length).indexOf("/") === -1, async route => {
    if (route.request().method() !== "GET") { await route.fallback(); return; }
    const id = decodeURIComponent(new URL(route.request().url()).pathname.slice((base + "/story/").length));
    if (originalIds.has(id)) { await route.fallback(); return; }
    const data = await fresh({ limit: 0 });
    const view = data.stories.find((candidate: any) => candidate.story.id === id);
    if (!view) { await route.fallback(); return; }
    await route.fulfill({ status: 200, json: { result: "ok", story: view, refs: {} } });
  });
}
