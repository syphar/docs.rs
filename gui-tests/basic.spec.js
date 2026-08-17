const { test, expect } = require("@playwright/test");

test("crate URLs show the expected docs.rs and rustdoc pages", async ({ page }) => {
    await page.goto("/sysinfo");
    await expect(page).toHaveURL(/\/sysinfo\/latest\/sysinfo\/$/);

    await page.goto("/sysinfo/0.23.5/sysinfo/index.html");
    await expect(page.locator(".title", { hasText: "sysinfo-0.23.5" })).toBeVisible();
    await expect(page.locator("#rustdoc_body_wrapper")).toBeVisible();

    await page.goto("/crate/sysinfo/0.23.5");
    await expect(page.locator("#rustdoc_body_wrapper")).toHaveCount(0);
    await expect(page.locator("#crate-title")).toContainText("sysinfo 0.23.5");
});
