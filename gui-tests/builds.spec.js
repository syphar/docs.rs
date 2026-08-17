const { test, expect } = require("@playwright/test");

const releases = ".recent-releases-container li a.release";
const successes = `${releases} span.fa-check`;
const failures = `${releases} span.fa-xmark`;

test("build status summaries expand into build details", async ({ page }) => {
    await page.goto("/crate/zbus/5.15.0/builds");

    const zbusSummary = page.locator(".recent-releases-container a.release");
    await expect(zbusSummary.locator("span.fa-triangle-exclamation")).toBeVisible();
    await expect(zbusSummary).toHaveAttribute("title", "Some builds failed");
    await zbusSummary.click();
    await expect(page.locator(releases)).toHaveCount(6);
    await expect(page.locator(successes)).toHaveCount(3);
    await expect(page.locator(failures)).toHaveCount(3);

    await page.goto("/crate/sysinfo/0.23.5/builds");
    const sysinfoSummary = page.locator(".recent-releases-container a.release");
    await expect(sysinfoSummary.locator("span.fa-check")).toBeVisible();
    await expect(sysinfoSummary).toHaveAttribute("title", "All builds succeeded");
    await sysinfoSummary.click();

    const buildCount = await page.locator(releases).count();
    expect(buildCount).toBeGreaterThan(0);
    await expect(page.locator(successes)).toHaveCount(buildCount);
    await expect(page.locator(failures)).toHaveCount(0);
});
