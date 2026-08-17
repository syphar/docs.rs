const { test, expect } = require("@playwright/test");

test("crate size details can be hovered and pinned", async ({ page }) => {
    await page.goto("/crate/sysinfo/latest");
    const size = page.locator(".package-page-container .package-menu .documented-info .size");
    const info = size.locator(".info");

    await expect(info).toHaveCSS("display", "none");
    await size.hover();
    await expect(info).toHaveCSS("display", "block");
    await page.locator("#clipboard").hover();
    await expect(info).toHaveCSS("display", "none");

    await size.click();
    await expect(info).toHaveCSS("display", "block");
    await page.locator("#clipboard").hover();
    await expect(info).toHaveCSS("display", "block");
    await page.locator("#main").click();
    await expect(info).toHaveCSS("display", "none");
});
