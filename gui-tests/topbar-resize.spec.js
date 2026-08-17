const { test, expect } = require("@playwright/test");
const { box } = require("./helpers");

const widths = [
    [1000, "inline"],
    [900, "inline"],
    [872, "inline"],
    [871, "none"],
    [768, "none"],
    [767, "none"],
    [568, "none"],
    [567, "none"],
    [310, "none"],
];

async function checkTopbar(page) {
    await page.setViewportSize({ width: 1100, height: 1000 });
    const topbar = page.locator("body > .nav-container");
    const topbarHeight = (await box(topbar)).height;

    for (const [width, titleDisplay] of widths) {
        await page.setViewportSize({ width, height: 1000 });
        await expect(page.locator(".nav-container > .container a > .title")).toHaveCSS(
            "display",
            titleDisplay,
        );
        expect(await page.evaluate(() => globalThis.innerWidth)).toBe(width);
        const clientWidth = await page.evaluate(() => {
            return globalThis.document.documentElement.clientWidth;
        });
        expect(clientWidth).toBe(width);
        expect((await box(topbar)).height).toBeCloseTo(topbarHeight, 0);
        const scrollWidth = await page.evaluate(() => globalThis.document.body.scrollWidth);
        expect(scrollWidth).toBe(width);
    }
}

test("topbar remains stable at responsive widths", async ({ page }) => {
    await page.goto("/sysinfo");
    await checkTopbar(page);
    await page.goto("/releases/search?query=sysinfo");
    await checkTopbar(page);
});
