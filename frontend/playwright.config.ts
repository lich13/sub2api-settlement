import fs from "node:fs";
import { defineConfig } from "@playwright/test";

const systemChrome =
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/*.e2e.ts",
  fullyParallel: false,
  workers: 1,
  timeout: 90_000,
  expect: { timeout: 15_000 },
  reporter: "line",
  use: {
    browserName: "chromium",
    viewport: { width: 1440, height: 900 },
    ...(!process.env.CI && fs.existsSync(systemChrome)
      ? { launchOptions: { executablePath: systemChrome } }
      : {}),
  },
});
