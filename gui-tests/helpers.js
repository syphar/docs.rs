const { expect } = require("@playwright/test");

async function box(locator) {
    await expect(locator).toBeVisible();
    const result = await locator.boundingBox();
    expect(result).not.toBeNull();
    return result;
}

async function expectPosition(locator, position) {
    const result = await box(locator);
    if (position.x !== undefined) {
        expect(result.x).toBeCloseTo(position.x, 0);
    }
    if (position.y !== undefined) {
        expect(result.y).toBeCloseTo(position.y, 0);
    }
}

async function checkTabs(page, names, centered) {
    const menu = page.locator(
        ".description-container .pure-menu-horizontal ul.pure-menu-list",
    );
    await expect(menu.locator(":scope > li")).toHaveCount(names.length);

    for (const [index, name] of names.entries()) {
        const active = menu.locator(":scope > li").nth(index).locator("a.pure-menu-active");
        await expect(active.locator(".title")).toHaveText(name);

        if (centered) {
            await expect(page.locator("body")).toHaveClass(/(?:^|\s)centered(?:\s|$)/);
        } else {
            await expect(page.locator("body")).not.toHaveClass(/(?:^|\s)centered(?:\s|$)/);
        }

        const container = page.locator("div.container").first();
        await expect(container).toHaveCSS("margin-top", "0px");
        await expect(container).toHaveCSS("margin-bottom", "0px");
        await expect(container).toHaveCSS("margin-left", centered ? "70px" : "0px");
        await expect(container).toHaveCSS("margin-right", centered ? "70px" : "0px");

        if (index + 1 < names.length) {
            await menu.locator(":scope > li").nth(index + 1).click();
        }
    }
}

module.exports = { box, checkTabs, expectPosition };
