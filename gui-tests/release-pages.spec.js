const { test } = require("@playwright/test");
const { checkTabs } = require("./helpers");

test("release pages use the centered layout", async ({ page }) => {
    await page.setViewportSize({ width: 1300, height: 800 });
    await page.goto("/releases");
    await checkTabs(
        page,
        ["Recent", "Stars", "Recent Failures", "Failures By Stars", "Activity", "Queue"],
        true,
    );
});
