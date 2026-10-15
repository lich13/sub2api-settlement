import type { Page } from "@playwright/test";
import { expect, test, type DemoServer } from "./demo.fixture";

async function loginAsAdmin(page: Page, demo: DemoServer, theme: "light" | "dark" = "light") {
  await page.goto(`${demo.origin}/settlement/sso/start?token=demo-1&user_id=1&theme=${theme}`);
  await expect(page.getByRole("heading", { name: "结算中心" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
}

async function assertNoHorizontalOverflow(page: Page) {
  const dimensions = await page.evaluate(() => ({
    viewport: window.innerWidth,
    document: document.documentElement.scrollWidth,
    body: document.body.scrollWidth,
  }));
  expect(dimensions.document, "document should fit the viewport").toBeLessThanOrEqual(dimensions.viewport);
  expect(dimensions.body, "body should fit the viewport").toBeLessThanOrEqual(dimensions.viewport);
}

test("admin can reach settlement controls at desktop and narrow widths in both themes", async ({ page, demo }) => {
  const variants = [
    { width: 1440, height: 900, theme: "light" as const },
    { width: 1440, height: 900, theme: "dark" as const },
    { width: 390, height: 844, theme: "light" as const },
    { width: 390, height: 844, theme: "dark" as const },
  ];

  for (const variant of variants) {
    await page.setViewportSize({ width: variant.width, height: variant.height });
    await loginAsAdmin(page, demo, variant.theme);

    const navigation = page.getByRole("navigation", { name: "结算导航" });
    await expect(navigation.getByRole("button", { name: "账号成本" })).toBeVisible();
    await expect(navigation.getByRole("button", { name: "我的账单" })).toBeVisible();
    await expect(navigation.getByRole("button", { name: "管理" })).toBeVisible();

    const accountRow = page.locator(".accounts-table tbody tr").filter({ hasText: "OpenAI Plus" });
    const settle = accountRow.getByRole("button", { name: "结算", exact: true });
    await expect(settle).toBeVisible();
    const bounds = await settle.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(variant.width);
    await assertNoHorizontalOverflow(page);
  }
});

test("admin settles an account cycle and voids the resulting settlement", async ({ page, demo }) => {
  await loginAsAdmin(page, demo);

  const accountRow = page.locator(".accounts-table tbody tr").filter({ hasText: "OpenAI Plus" });
  await accountRow.getByRole("button", { name: "结算", exact: true }).click();

  let dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: /结算预览/ })).toBeVisible();
  const settle = dialog.getByRole("button", { name: "确认结算", exact: true });
  await expect(settle).toBeEnabled();
  await settle.click();
  await expect(dialog).toHaveCount(0);

  await page.getByRole("navigation", { name: "结算导航" }).getByRole("button", { name: "管理" }).click();
  await page.getByRole("button", { name: "周期", exact: true }).click();
  let cycleRow = page.locator(".history-table tbody tr").filter({ hasText: "OpenAI Plus" });
  await expect(cycleRow).toContainText("已结算");
  await cycleRow.getByRole("button", { name: "作废", exact: true }).click();

  dialog = page.getByRole("dialog");
  await expect(dialog).toContainText(/作废.*本次结算/);
  await dialog.getByRole("button", { name: "确认", exact: true }).click();
  await expect(dialog).toHaveCount(0);

  cycleRow = page.locator(".history-table tbody tr").filter({ hasText: "OpenAI Plus" });
  await expect(cycleRow.getByRole("button", { name: "结算", exact: true })).toBeVisible();
});

test("admin records a note when settling a netting balance", async ({ page, demo }) => {
  await loginAsAdmin(page, demo);
  await page.getByRole("navigation", { name: "结算导航" }).getByRole("button", { name: "我的账单" }).click();

  const pendingBalance = page.locator(".netting-bill").filter({ hasText: "净应付" }).first();
  await expect(pendingBalance.getByRole("button", { name: "确认转账", exact: true })).toBeVisible();
  await pendingBalance.getByRole("button", { name: "确认转账", exact: true }).click();

  const note = "browser-e2e-netting-note";
  let dialog = page.getByRole("dialog");
  await dialog.getByLabel("备注（选填）").fill(note);
  await dialog.getByRole("button", { name: "确认", exact: true }).click();
  await expect(dialog).toHaveCount(0);

  const settledBalance = page.locator(".netting-bill").filter({ hasText: "转账结清" }).first();
  await expect(settledBalance).toBeVisible();
  await settledBalance.getByRole("button", { name: /明细/ }).click();
  dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "对冲明细" })).toBeVisible();
  await expect(dialog).toContainText(`备注：${note}`);
});

test("failed account deletion can be retried successfully from the management view", async ({ page, demo }) => {
  await loginAsAdmin(page, demo);

  const accountRow = page.locator(".accounts-table tbody tr").filter({ hasText: "OpenAI Plus" });
  await accountRow.getByRole("button", { name: "结算", exact: true }).click();
  let dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: /结算预览/ })).toBeVisible();
  await dialog.getByLabel("出单后删除账号").check();
  const deletionScope = dialog.getByLabel(/确认删除以上/);
  await expect(deletionScope).toBeVisible();
  await deletionScope.check();
  await dialog.getByRole("button", { name: "确认结算", exact: true }).click();
  await expect(dialog).toHaveCount(0);

  await page.getByRole("navigation", { name: "结算导航" }).getByRole("button", { name: "管理" }).click();
  await page.getByRole("button", { name: "账号删除", exact: true }).click();
  const deletionRow = page.locator(".deletion-row").filter({ hasText: "OpenAI Plus" });
  await expect(deletionRow).toContainText("删除未完成", { timeout: 45_000 });
  await deletionRow.getByRole("button", { name: "重试删除", exact: true }).click();

  dialog = page.getByRole("dialog");
  await expect(dialog).toContainText(/重试删除以下/);
  await dialog.getByRole("button", { name: "确认", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByText("暂无待处理删除", { exact: true })).toBeVisible({ timeout: 45_000 });
});
