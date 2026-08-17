const { test, expect } = require("@playwright/test");

test("crate dropdown items keep equal padding at responsive widths", async ({ page }) => {
    await page.goto("/sysinfo");

    const dropdown = page.locator("#nav-search-form .crate-dropdown");
    await expect(dropdown).not.toHaveClass(/pure-menu-active/);
    await dropdown.click();
    await expect(dropdown).toHaveClass(/pure-menu-active/);

    const items = page.locator(".package-details-menu > ul > .pure-menu-item");
    await expect(items).toHaveCount(4);

    for (const width of [900, 730, 690, 510]) {
        await page.setViewportSize({ width, height: 800 });
        const padding = await items.first().locator(":scope > *").evaluate(element => {
            return globalThis.getComputedStyle(element).padding;
        });
        for (const item of await items.locator(":scope > *").all()) {
            await expect(item).toHaveCSS("padding", padding);
        }
    }
});
