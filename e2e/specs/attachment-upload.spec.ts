import {
  test, expect, seedToken, openProject, projectSlug, createStory,
  cleanUpCreatedStories,
} from "./support";

// The upload API is browser infrastructure for SH-391/SH-392. Exercise a real
// Blob and the browser's cookie/Fetch behavior without introducing their UI.
cleanUpCreatedStories("Alpha Project");

test("a browser Blob uploads losslessly using the named-token cookie", async ({ page, request }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const id = await createStory(page, "Browser image upload");
  const slug = await projectSlug(request, "Alpha Project");
  const result = await page.evaluate(async ({ slug, id }) => {
    // A real browser-encoded image, padded past the ordinary JSON-body limit.
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 1;
    const png = await new Promise<Blob>((resolve, reject) => canvas.toBlob(
      blob => blob ? resolve(blob) : reject(new Error("PNG encoding failed")), "image/png",
    ));
    const blob = new Blob([png, new Uint8Array(64 * 1024)], { type: "image/png" });
    const bytes = new Uint8Array(await blob.arrayBuffer());
    const path = `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}`;
    const response = await fetch(`${path}/attachments`, {
      method: "POST",
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Attachment-Name": encodeURIComponent("café + screenshot.png"),
      },
      body: blob,
    });
    const body = await response.json();
    const read = await fetch(path, { headers: { "X-Storyhook": "1" } });
    const stored = await read.json();
    const attachmentId = body.story.story.attachments[0].id;
    const download = await fetch(`${path}/attachments/${attachmentId}`);
    const downloaded = new Uint8Array(await download.arrayBuffer());
    const identical = downloaded.length === bytes.length
      && downloaded.every((byte, index) => byte === bytes[index]);
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    const sha256 = Array.from(new Uint8Array(digest), b => b.toString(16).padStart(2, "0")).join("");
    return {
      status: response.status, body, stored, byteLength: bytes.length, sha256,
      downloadStatus: download.status, identical,
    };
  }, { slug, id });
  expect(result.status).toBe(201);
  expect(result.body.result).toBe("ok");
  expect(result.downloadStatus).toBe(200);
  expect(result.identical).toBe(true);
  expect(result.stored).toMatchObject({ result: "ok" });
  expect(result.body.story.story.attachments).toEqual([
    expect.objectContaining({
      id: 1,
      name: "café + screenshot.png",
      media_type: "png",
      byte_len: result.byteLength,
      sha256: result.sha256,
    }),
  ]);
  expect(result.stored.story.story.attachments).toEqual(result.body.story.story.attachments);
});
