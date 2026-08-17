const { test, expect } = require("@playwright/test");
const { box } = require("./helpers");

test("long license text wraps inside the crate dropdown", async ({ page }) => {
    await page.goto("/sysinfo");
    const menu = page.locator(".crate-dropdown");
    const submenu = menu.locator(".pure-menu-children");
    const license = submenu.locator(".license");
    await menu.click();
    await expect(submenu).toHaveCSS("display", "block");

    const original = await box(license);
    const menuBox = await box(submenu);
    expect(menuBox.width - 2).toBeCloseTo(original.width, 0);

    await license.evaluate(element => {
        element.innerHTML += "OR LicenseRef-Slint-Royalty-free-2.0 OR " +
            "LicenseRef-Slint-Software-3.0 OR blablablablabla";
    });
    const changed = await box(license);
    expect(changed.width).toBeCloseTo(original.width, 0);
    expect(changed.height).toBeGreaterThan(original.height);
});
