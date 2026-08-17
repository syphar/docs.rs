const { test, expect } = require("@playwright/test");

async function setTheme(page, theme) {
    await page.evaluate(value => {
        const oldValue = globalThis.localStorage.getItem("rustdoc-theme");
        globalThis.localStorage.setItem("rustdoc-theme", value);
        globalThis.dispatchEvent(new globalThis.StorageEvent("storage", {
            key: "rustdoc-theme",
            newValue: value,
            oldValue,
            storageArea: globalThis.localStorage,
        }));
    }, theme);
}

test("docs.rs pages follow the rustdoc theme", async ({ page }) => {
    const themes = [
        ["light", "rgb(255, 255, 255)"],
        ["ayu", "rgb(15, 20, 25)"],
    ];

    for (const [theme, color] of themes) {
        await page.goto("/sysinfo");
        await setTheme(page, theme);
        await expect(page.locator(".nav-container")).toHaveCSS("background-color", color);

        await page.goto("/crate/sysinfo");
        const storedTheme = await page.evaluate(() => {
            return globalThis.localStorage.getItem("rustdoc-theme");
        });
        expect(storedTheme).toBe(theme);
        await expect(page.locator("body")).toHaveCSS("background-color", color);
    }
});
