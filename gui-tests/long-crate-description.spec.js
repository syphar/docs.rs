const { test, expect } = require("@playwright/test");
const { box } = require("./helpers");

test("long crate descriptions do not overflow or jump on hover", async ({ page }) => {
    await page.goto("/releases/search?query=sysinfo");
    const description = page.locator(".recent-releases-container .description");

    await description.evaluate(element => {
        element.textContent = "SOME/VERYVERY/LONG/SEQUENCE/OF/TEXT/SEPARATED/BY/SLASHES\n" +
            "Anotherveryveryveryveryveryveryveryveryveryveryveryveryveryveryveryvery" +
            "veryveryveryveryveryveryveryveryveryveryveryveryveryveryveryveryveryvery" +
            "veryveryveryveryveryveryveryveryveryverylongWord";
    });
    await page.setViewportSize({ width: 567, height: 800 });
    const descriptionWidth = await description.evaluate(element => element.scrollWidth);
    expect(descriptionWidth).toBeLessThanOrEqual(567);

    await page.setViewportSize({ width: 1200, height: 800 });
    const release = page.locator(".recent-releases-container a.release");
    await description.evaluate(element => {
        element.textContent = "A very long crate description that should remain on one line " +
            "when hovered instead of expanding and moving nearby release rows";
    });
    const height = (await box(release)).height;
    await description.hover();
    expect((await box(release)).height).toBeCloseTo(height, 0);
});
