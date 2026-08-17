const { test } = require("@playwright/test");
const { checkTabs } = require("./helpers");

test("crate pages use the full-width layout", async ({ page }) => {
    await page.setViewportSize({ width: 1300, height: 800 });
    await page.goto("/crate/sysinfo");
    await checkTabs(page, [" Crate", " Source", " Builds", "Feature flags"], false);
});
