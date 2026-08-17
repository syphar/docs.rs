const { test, expect } = require("@playwright/test");

test("release and build links use the expected colors", async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => globalThis.localStorage.setItem("rustdoc-theme", "light"));
    await page.reload();

    await expect(page.locator("a[href='/releases']")).toHaveCSS("color", "rgb(0, 0, 0)");
    await expect(page.locator("a[href='/releases/feed']")).toHaveCSS(
        "color",
        "rgb(0, 0, 0)",
    );
    for (const link of await page.locator("li a.release .name").all()) {
        await expect(link).toHaveCSS("color", "rgb(77, 118, 174)");
    }

    await page.goto("/crate/sysinfo/0.23.5/builds");
    for (const link of await page.locator("li a.release > div").all()) {
        await expect(link).toHaveCSS("color", "rgb(0, 0, 0)");
    }
});
