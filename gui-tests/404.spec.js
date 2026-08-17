const { test, expect } = require("@playwright/test");

test("404 pages contain recovery information", async ({ page }) => {
    await page.goto("/non-existing-crate");
    await expect(page.locator("#crate-title")).toHaveText(
        "The requested crate does not exist",
    );

    await page.goto("/sysinfo/latest/sysinfo/removed_module/index.html");
    await expect(page.locator("#crate-title")).toHaveText("This page does not exist");
    await expect(page.locator(".description")).toContainText(
        "The latest version of `sysinfo` (0.23.5) exists",
    );

    const links = page.locator("#recovery-links a");
    await expect(links).toHaveCount(2);
    await expect(links.nth(0)).toHaveText("Documentation home for this version");
    await expect(links.nth(0)).toHaveAttribute("href", "/sysinfo/latest/sysinfo/");
    await expect(links.nth(1)).toHaveText("All versions of sysinfo");
    await expect(links.nth(1)).toHaveAttribute("href", "/crate/sysinfo/latest");
});
