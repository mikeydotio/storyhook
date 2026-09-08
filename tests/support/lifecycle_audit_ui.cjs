/** Exercise the generated offline report in a real browser, without a daemon. */
const assert = require('node:assert/strict');
const { readFile, mkdtemp, rm } = require('node:fs/promises');
const { join, resolve } = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require('../../e2e/node_modules/playwright');
const { expect } = require('../../e2e/node_modules/@playwright/test');

async function main() {
  const browser = await chromium.launch({ headless: true });
  const scratch = await mkdtemp('/tmp/SH-560-browser-');
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const requests = [];
    page.on('request', request => requests.push(request.url()));
    await page.goto(pathToFileURL(resolve('docs/reports/SH-560-lifecycle-audit.html')).href);
    assert.equal(await page.locator('.finding').count(), 14);
    assert.equal(await page.locator('#cohort-table tbody tr').count(), 36);
    assert.match(await page.locator('#selection-status').innerText(), /6 selected/);
    await page.getByRole('button', { name: 'Clear selections' }).click();
    assert.equal(await page.getByRole('button', { name: 'Download selections' }).isDisabled(), true);
    await page.getByRole('searchbox', { name: 'Search findings' }).fill('project export');
    assert.equal(await page.locator('.finding').count(), 1);
    await page.locator('.select-finding').check();
    await page.getByRole('searchbox').fill('');
    await page.getByRole('combobox', { name: 'Repair status' }).selectOption('fixed');
    assert.equal(await page.locator('.finding').count(), 4);
    assert.match(await page.locator('#selection-status').innerText(), /1 selected/);
    const downloadPromise = page.waitForEvent('download');
    await page.getByRole('button', { name: 'Download selections' }).click();
    const download = await downloadPromise;
    const destination = join(scratch, 'selection.json');
    await download.saveAs(destination);
    const selected = JSON.parse(await readFile(destination, 'utf8'));
    assert.deepEqual(selected.findings.map(f => f.id), ['F07']);
    assert.equal(selected.findings[0].story_id, 'SH-608');
    await page.getByRole('combobox', { name: 'Repair status' }).selectOption('all');
    await page.getByRole('button', { name: 'Select filed set' }).click();
    assert.equal(await page.locator('.select-finding:checked').count(), 6);
    await page.locator('#F01 a[href="#event-21455"]').click();
    await expect(page.getByRole('dialog')).toBeVisible();
    assert.match(await page.locator('#evidence-text').innerText(), /PID 19720/);
    assert.match(await page.locator('#evidence-meta').innerText(), /Command:/);
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog')).toBeHidden();
    await page.locator('#cohort-table a[href="#story-SH-591"]').click();
    await expect(page.locator('#story-SH-591')).toHaveAttribute('open', '');
    // The selected evidence is real report data; no runtime behavior is mocked.
    await page.locator('#story-SH-591').getByText(/source events/).click();
    await page.locator('#story-SH-591').getByRole('link', { name: 'Event 21768', exact: true }).click();
    await expect(page.getByRole('dialog')).toBeVisible();
    assert.match(await page.locator('#evidence-meta').innerText(), /web:move/);
    assert.match(await page.locator('#evidence-meta').innerText(), /web:user/);
    await page.getByRole('button', { name: 'Close evidence' }).click();
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(pathToFileURL(resolve('docs/reports/SH-560-lifecycle-audit.html')).href);
    const overflow = await page.evaluate(() => [...document.querySelectorAll('body *')]
      .filter(element => (element.getBoundingClientRect().right > innerWidth ||
        (element.clientWidth > 0 && element.scrollWidth > element.clientWidth)) &&
        !element.closest('.table-wrap') && !element.closest('dialog'))
      .map(element => ({ tag: element.tagName, id: element.id, text: element.textContent.slice(0, 90) })));
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true,
      JSON.stringify(overflow));
    assert.deepEqual(errors, []);
    assert.equal(requests.every(url => url.startsWith('file:')), true);
    await page.screenshot({ path: '/tmp/SH-560-report-mobile.png', fullPage: true });
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.screenshot({ path: '/tmp/SH-560-report-desktop.png', fullPage: false });
    console.log('Offline report: filtering, selection, export, evidence, keyboard, mobile, and zero-network checks passed.');
  } finally {
    await browser.close();
    await rm(scratch, { recursive: true, force: true });
  }
}

main().catch(error => { console.error(error); process.exitCode = 1; });
