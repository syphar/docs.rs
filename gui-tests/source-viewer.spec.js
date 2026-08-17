const { test, expect } = require("@playwright/test");
const { box } = require("./helpers");

test("source viewer controls resize the code area", async ({ page }) => {
    await page.goto("/crate/sysinfo/latest/source/Cargo.toml");
    await page.evaluate(() => {
        globalThis.localStorage.setItem("rustdoc-theme", "dark");
        globalThis.localStorage.setItem("rustdoc-use-system-theme", "false");
    });
    await page.reload();
    await page.setViewportSize({ width: 800, height: 800 });

    const docLink = page.locator(".doc-link");
    const iconHeight = (await box(docLink.locator(":scope > span"))).height;
    const buttonHeight = (await box(docLink)).height;
    await expect(docLink).toHaveCSS("padding-top", "10px");
    await expect(docLink).toHaveCSS("padding-bottom", "10px");
    expect(buttonHeight - 20).toBeLessThan(iconHeight * 2);

    const toggle = page.locator(".toggle-source");
    const button = toggle.locator(":scope > button");
    await expect(toggle).toHaveCSS("border-color", "rgb(78, 78, 78)");
    await expect(toggle).toHaveCSS("cursor", "pointer");
    await expect(button).toHaveCSS("cursor", "pointer");
    await button.hover();
    await expect(toggle).toHaveCSS("border-color", "rgb(192, 192, 192)");
    await expect(toggle).toHaveCSS("cursor", "pointer");
    await expect(button).toHaveCSS("cursor", "pointer");

    const sideMenu = page.locator("#side-menu");
    const source = page.locator("#source-code-container");
    const originalSideMenuWidth = (await box(sideMenu)).width;
    const originalSourceWidth = (await box(source)).width;

    await button.click();
    await expect(sideMenu).toHaveClass(/collapsed/);
    const collapsedSideMenuWidth = (await box(sideMenu)).width;
    const expandedSourceWidth = (await box(source)).width;
    expect(originalSideMenuWidth).toBeGreaterThan(collapsedSideMenuWidth);
    expect(originalSourceWidth).toBeLessThan(expandedSourceWidth);
    expect(expandedSourceWidth + collapsedSideMenuWidth).toBeCloseTo(
        originalSourceWidth + originalSideMenuWidth,
        0,
    );
});
