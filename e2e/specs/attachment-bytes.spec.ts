import { test } from "./support";
import { expectCookieAttachment } from "./attachment-fixture";

test("attachment bytes decode through the dashboard cookie without custom headers", async ({ page }) => {
  await expectCookieAttachment(page);
});
