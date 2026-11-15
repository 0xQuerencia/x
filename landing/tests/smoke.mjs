import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { chromium } from "playwright";

const origin = "http://127.0.0.1:4173";
const server = spawn("npm", ["run", "preview", "--", "--host", "127.0.0.1", "--port", "4173", "--strictPort"], { stdio: "inherit" });
let browser;
try {
  let ready = false;
  for (let i = 0; i < 60; i++) {
    try { const response = await fetch(origin); if (response.ok) { ready = true; break; } } catch {}
    await sleep(250);
  }
  assert.ok(ready, "Vite preview started");
  browser = await chromium.launch({ headless: true });

  for (const viewport of [{ width: 1366, height: 768 }, { width: 390, height: 844 }]) {
    const context = await browser.newContext({ viewport, reducedMotion: "reduce" });
    await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin });
    const page = await context.newPage();
    const jsErrors = [];
    page.on("pageerror", error => jsErrors.push(error.message));
    await page.goto(origin, { waitUntil: "networkidle" });
    assert.equal(await page.title(), "X: talk and it types");
    assert.equal(await page.locator("main section").count(), 9);
    assert.equal(await page.locator("#hero-tabs button").count(), 4);
    assert.equal(await page.locator("#count").textContent(), "1 / 6");
    assert.equal(await page.locator(".faq details").count(), 10);
    assert.ok(await page.locator("#field img.layer.on").isVisible());

    await page.locator('#hero-tabs button[data-i="models"]').click();
    await page.waitForFunction(() => document.querySelector("#field")?.getAttribute("aria-label")?.includes("model picker"));
    assert.equal(await page.locator('#hero-tabs button[data-i="models"]').getAttribute("aria-selected"), "true");
    await page.locator('#hero-tabs button[data-i="result"]').click();
    await page.waitForFunction(() => document.querySelector("#field")?.getAttribute("aria-label")?.includes("browser page"));

    await page.locator('.codetabs button[data-c="aur"]').click();
    assert.ok((await page.locator('.code:not([hidden]) pre').textContent()).includes("makepkg -si"));
    await page.locator(".code:not([hidden]) .copy").click();
    await page.waitForFunction(() => document.querySelector(".code:not([hidden]) .copy")?.textContent === "copied");
    assert.ok((await page.evaluate(() => navigator.clipboard.readText())).includes("makepkg -si"));

    await page.locator("#next").click();
    assert.equal(await page.locator("#count").textContent(), "2 / 6");
    await page.locator("#prev").click();
    assert.equal(await page.locator("#count").textContent(), "1 / 6");

    await page.locator("#faq details").first().locator("summary").click();
    assert.ok(await page.locator("#faq details").first().evaluate(el => el.open));

    await page.locator("#theme").click();
    assert.equal(await page.locator("html").getAttribute("data-theme"), "light");
    await page.reload({ waitUntil: "networkidle" });
    assert.equal(await page.locator("html").getAttribute("data-theme"), "light");

    const css = await page.locator("body").evaluate(el => getComputedStyle(el).fontFamily);
    assert.match(css.toLowerCase(), /figtree/);
    assert.deepEqual(jsErrors, []);
    await context.close();
  }
  console.log("X landing smoke checks passed (desktop + mobile)");
} finally {
  await browser?.close();
  server.kill("SIGTERM");
}
