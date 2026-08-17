const { test, expect } = require("@playwright/test");
const { box, expectPosition } = require("./helpers");

test("rustdoc sidebars remain below the docs.rs navigation", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 1000 });
    await page.goto("/sysinfo");

    const topbarHeight = (await box(page.locator(".rustdoc-page .nav-container"))).height;
    await expectPosition(page.locator(".rustdoc .sidebar"), { x: 0, y: topbarHeight });

    await page.setViewportSize({ width: 600, height: 1000 });
    const rustdocTopbar = page.locator("rustdoc-topbar");
    await expect(rustdocTopbar).toBeVisible();
    await expectPosition(rustdocTopbar, { x: 0, y: topbarHeight });
    await rustdocTopbar.locator(".sidebar-menu-toggle").click();
    const mobileSidebar = page.locator(".rustdoc .sidebar.shown");
    await expect(mobileSidebar).toBeVisible();
    const rustdocTopbarHeight = (await box(rustdocTopbar)).height;
    await expectPosition(mobileSidebar, {
        x: 0,
        y: topbarHeight + rustdocTopbarHeight,
    });

    await page.setViewportSize({ width: 1000, height: 1000 });
    const sourceLink = page.locator(".main-heading a.src");
    await expect(sourceLink).toBeVisible();
    await sourceLink.click();
    await expect(page.locator("#src-sidebar")).toBeVisible();
    const sidebar = page.locator(".rustdoc .sidebar");
    await expectPosition(sidebar, { x: 0, y: topbarHeight });
    await page.locator("#sidebar-button a").click();
    await expect(page.locator(".src-sidebar-expanded")).toBeVisible();
    await expectPosition(sidebar, { x: 0, y: topbarHeight });

    await page.setViewportSize({ width: 600, height: 1000 });
    await expectPosition(sidebar, { x: 0, y: topbarHeight });
});
