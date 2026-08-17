const { test, expect } = require("@playwright/test");
const { box } = require("./helpers");

test("long crate names wrap without overflowing on hover", async ({ page }) => {
    await page.goto("/");
    const name = page.locator("ul a.release .name").first();
    await name.evaluate(element => {
        element.textContent = "blabla_under_something_longname_right-0.7.0";
    });

    await page.setViewportSize({ width: 1200, height: 600 });
    await name.hover();
    await expect(name).toHaveCSS("line-height", "22.4px");
    expect(Math.round((await box(name)).height)).toBe(45);

    await page.setViewportSize({ width: 800, height: 600 });
    expect(Math.round((await box(name)).height)).toBe(67);
});
