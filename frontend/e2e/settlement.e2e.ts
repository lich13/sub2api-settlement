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
    { width: 375, height: 667, theme: "light" as const },
    { width: 375, height: 667, theme: "dark" as const },
    { width: 360, height: 640, theme: "light" as const },
    { width: 360, height: 640, theme: "dark" as const },
  ];

  for (const variant of variants) {
    await page.setViewportSize({ width: variant.width, height: variant.height });
    await loginAsAdmin(page, demo, variant.theme);

    const versionBadge = page.locator(".topbar .app-version");
    await expect(versionBadge).toBeVisible();
    await expect(versionBadge).toHaveText(/^v\d+\.\d+\.\d+$/);
    const headingBox = await page.getByRole("heading", { name: "结算中心" }).boundingBox();
    const versionBox = await versionBadge.boundingBox();
    const brandBox = await page.locator(".topbar .brand").boundingBox();
    const rightBox = await page.locator(".topbar .header-right").boundingBox();
    expect(headingBox).not.toBeNull();
    expect(versionBox).not.toBeNull();
    expect(brandBox).not.toBeNull();
    expect(rightBox).not.toBeNull();
    expect(versionBox!.height).toBeLessThanOrEqual(20);
    expect(Math.abs((versionBox!.y + versionBox!.height / 2) - (headingBox!.y + headingBox!.height / 2))).toBeLessThanOrEqual(6);
    expect(brandBox!.x + brandBox!.width).toBeLessThanOrEqual(rightBox!.x);

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

  await page.route("**/settlement/api/v1/me", async (route) => {
    const response = await route.fetch();
    const json = await response.json();
    await route.fulfill({
      response,
      json: {
        ...json,
        name: "组织运营结算中心超长管理员名称验证_admin_super_long_identifier@example.invalid",
      },
    });
  });

  for (const width of [768, 361]) {
    await page.setViewportSize({ width, height: 900 });
    await loginAsAdmin(page, demo, "light");
    await expect(page.locator(".topbar .identity-name")).toBeVisible();
    const brandBox = await page.locator(".topbar .brand").boundingBox();
    const rightBox = await page.locator(".topbar .header-right").boundingBox();
    expect(brandBox).not.toBeNull();
    expect(rightBox).not.toBeNull();
    expect(brandBox!.x + brandBox!.width).toBeLessThanOrEqual(rightBox!.x);
    if (width >= 761) {
      const badgeBox = await page.locator(".topbar .identity small").boundingBox();
      expect(badgeBox).not.toBeNull();
      expect(badgeBox!.height).toBeLessThanOrEqual(24);
    }
    await assertNoHorizontalOverflow(page);
  }
  await page.unroute("**/settlement/api/v1/me");
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
  await page.getByRole("button", { name: "全部订单", exact: true }).click();
  const rawSummary = page.locator(".raw-orders > summary");
  const rawChevron = page.locator(".raw-orders > summary > svg");
  await expect(rawChevron).toBeVisible();
  const rawSummaryBox = await rawSummary.boundingBox();
  const rawChevronBox = await rawChevron.boundingBox();
  expect(rawSummaryBox).not.toBeNull();
  expect(rawChevronBox).not.toBeNull();
  expect(rawChevronBox!.x).toBeGreaterThan(rawSummaryBox!.x + rawSummaryBox!.width - 36);
  await page.getByRole("button", { name: "操作记录", exact: true }).click();
  await expect(page.locator(".audit-list details > summary > svg").first()).toBeVisible();
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
  await expect(dialog.locator(".deletion-targets")).toContainText("OpenAI Plus");
  await expect(dialog.getByLabel(/确认删除以上/)).toHaveCount(0);
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

test("admin creates and reconfigures an account group without retired first-period prompts", async ({ page, demo }) => {
  await loginAsAdmin(page, demo);

  await page.getByRole("button", { name: "新建账号组", exact: true }).click();
  let dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "新建账号组" })).toBeVisible();
  await dialog.getByLabel("账号组名称").fill("E2E 供应商组");
  await dialog.getByLabel("本期准确总成本（元）").fill("18.00");
  await dialog.getByRole("button", { name: "全选", exact: true }).click();
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await expect(dialog).toHaveCount(0);

  const group = page.locator(".vendor-group").filter({ hasText: "E2E 供应商组" });
  await expect(group).toContainText("进行中");
  await expect(group).toContainText("¥18.00");
  await expect(group).not.toContainText("待配置首期");
  await group.locator("summary").click();
  await group.getByRole("button", { name: "管理账号组", exact: true }).click();

  dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "账号组管理" })).toBeVisible();
  await expect(dialog).not.toContainText("配置首期");
  await dialog.getByRole("button", { name: "整组配置", exact: true }).click();
  await dialog.getByLabel("本期准确总成本（元）").fill("24.00");
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(group).toContainText("¥24.00");
});
