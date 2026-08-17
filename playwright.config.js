const { defineConfig, devices } = require("@playwright/test");

module.exports = defineConfig({
    testDir: "./gui-tests",
    outputDir: "./ignored/playwright-results",
    reporter: process.env.CI ? "github" : "list",
    use: {
        baseURL: process.env.SERVER_URL || "http://127.0.0.1:3000",
        screenshot: "only-on-failure",
        trace: "retain-on-failure",
        ...devices["Desktop Chrome"],
    },
    webServer: undefined,
});
