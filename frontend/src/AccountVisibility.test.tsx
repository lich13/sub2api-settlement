// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./main";
import type { Account, Actor, Catalog, Cycle, Overview, Order, Preview } from "./types";

let root: Root | undefined;
let host: HTMLDivElement | undefined;
const nativeShowModal = HTMLDialogElement.prototype.showModal;
const nativeClose = HTMLDialogElement.prototype.close;

function response(data: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
  } as Response;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const actor: Actor = { id: 9, name: "Admin", role: "admin", csrf: "csrf-fixture" };

function cycle(overrides: Partial<Cycle> = {}): Cycle {
  return {
    id: 101,
    account_id: 1,
    name: "Removed account",
    cost_fen: 5_000,
    creditor_id: 9,
    start: "2026-09-01T00:00:00Z",
    end: null,
    status: "open",
    version: 1,
    settlement_id: null,
    cost_kind: "exact",
    ...overrides,
  };
}

const removedCycle = cycle();
const activeCycle = cycle({ id: 202, account_id: 2, name: "Active account" });

function account(value: Cycle): Account {
  return {
    id: value.account_id,
    name: value.name,
    platform: "claude",
    kind: "paid",
    deleted: false,
    profile_version: 1,
    cycle: value,
    mine: { user_id: 9, user_name: "Admin", consumption: "1.00", amount_fen: 500, share: "0.1", self_covered: false },
    creditor_name: "Admin",
    total_consumption: "1.00",
    record_count: 2,
    can_settle: true,
  };
}

const overview: Overview = {
  account_revision: "2026-10-01T00:00:00.000Z",
  accounts: [account(removedCycle), account(activeCycle)],
  estimated_fen: 0,
  pending_fen: 0,
  receivable_fen: 0,
  submitted_fen: 0,
  last_sync: "2026-10-01T00:00:00Z",
  sync_error: null,
  coverage_from: "2026-01-01T00:00:00Z",
};

const catalog: Catalog = {
  accounts: [removedCycle, activeCycle].map(value => ({
    id: value.account_id,
    name: value.name,
    platform: "claude",
    status: "active",
    deleted: false,
    created_at: "2026-01-01T00:00:00Z",
  })),
  users: [{ id: 9, name: "Admin" }],
  coverage_from: "2026-01-01T00:00:00Z",
};

const historicalOrder: Order = {
  id: 700,
  settlement_id: 77,
  cycle_id: 101,
  payer_id: 9,
  creditor_id: 8,
  payer_name: "Admin",
  creditor_name: "Former creditor",
  account_name: "Removed account historical bill",
  amount_fen: 500,
  status: "submitted",
  batch_id: null,
  version: 1,
  start: "2026-09-01T00:00:00Z",
  end: "2026-10-01T00:00:00Z",
  created_at: "2026-10-01T00:00:00Z",
  netting_payment_id: null,
  drift: false,
};

function settlementPreview(value: Cycle = removedCycle): Preview {
  return {
    id: `preview-${value.id}`,
    expires_at: "2026-10-02T00:00:00Z",
    cycles: [{
      cycle: value,
      end: "2026-10-01T00:00:00Z",
      total_consumption: "1.00",
      allocations: [],
      record_count: 2,
      can_settle: true,
      start_confirmed: true,
    }],
  };
}

function summarizedAccount(
  id: number,
  name: string,
  kind: string,
  overrides: Partial<Account> = {},
): Account {
  return {
    id,
    name,
    platform: "claude",
    kind,
    deleted: false,
    profile_version: kind === "unconfigured" ? null : 1,
    cycle: null,
    mine: null,
    creditor_name: "Admin",
    total_consumption: "0",
    record_count: 0,
    can_settle: false,
    ...overrides,
  };
}

function stateSnapshot(accounts: { id: number; name: string }[], revision = "2026-10-01T00:00:00.000Z") {
  return {
    revision,
    accounts,
    active_ids: accounts.map(value => value.id),
    checked_at: revision,
    error: null,
  };
}

function catalogFor(accounts: Account[]): Catalog {
  return {
    accounts: accounts.map(value => ({
      id: value.id,
      name: value.name,
      platform: value.platform,
      status: "active",
      deleted: value.deleted,
      created_at: "2026-01-01T00:00:00Z",
    })),
    users: [{ id: 9, name: "Admin" }],
    coverage_from: "2026-01-01T00:00:00Z",
  };
}

function setupFetch(options: {
  currentActor?: Actor;
  initialState?: unknown;
  stateUpdates?: Array<Response | Promise<Response>>;
  initialOverview?: Overview;
  overviewUpdates?: Array<Response | Promise<Response>>;
  initialCatalog?: Catalog;
  catalogUpdates?: Array<Response | Promise<Response>>;
  cycleDetail?: unknown;
  previewCycle?: Cycle;
} = {}) {
  let stateCalls = 0;
  let overviewCalls = 0;
  let catalogCalls = 0;
  const nextState = deferred<Response>();
  const fetcher = vi.fn<typeof fetch>(async input => {
    const path = String(input);
    if (path.endsWith("/me")) return response(options.currentActor || actor);
    if (path.endsWith("/accounts/state")) {
      stateCalls += 1;
      if (stateCalls === 1) return response(options.initialState || stateSnapshot([{ id: 1, name: "Removed account" }, { id: 2, name: "Active account" }]));
      return options.stateUpdates?.[stateCalls - 2] || nextState.promise;
    }
    if (path.endsWith("/overview")) {
      overviewCalls += 1;
      return options.overviewUpdates?.[overviewCalls - 2] || response(options.initialOverview || overview);
    }
    if (path.endsWith("/catalog")) {
      catalogCalls += 1;
      return options.catalogUpdates?.[catalogCalls - 2] || response(options.initialCatalog || catalog);
    }
    if (path.endsWith("/inbox")) return response({ items: [], unread: 0 });
    if (path.endsWith("/expenses")) return response({ expenses: [] });
    if (path.endsWith("/previews")) return response(settlementPreview(options.previewCycle));
    if (path.endsWith("/history")) return response({ cycles: [removedCycle, activeCycle], audit: [] });
    if (path.endsWith("/account-deletions")) return response({ items: [] });
    if (path.endsWith("/orders?scope=mine")) return response({ orders: [historicalOrder] });
    if (/\/cycles\/\d+$/.test(path)) return response(options.cycleDetail || {
      cycle: removedCycle,
      end: "2026-10-01T00:00:00Z",
      total_consumption: "1.00",
      allocations: [],
      record_count: 2,
    });
    if (path.includes("/netting/balances")) return response({ balances: [] });
    if (path.includes("/netting/payments")) return response({ payments: [] });
    return response({});
  });
  vi.stubGlobal("fetch", fetcher);
  return {
    fetcher,
    nextState,
    stateCalls: () => stateCalls,
    overviewCalls: () => overviewCalls,
    catalogCalls: () => catalogCalls,
  };
}

async function renderApp() {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(<App />);
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

function rowFor(name: string) {
  const row = Array.from(host!.querySelectorAll("tbody tr"))
    .find(element => element.textContent?.includes(name));
  if (!row) throw new Error(`row not found: ${name}`);
  return row;
}

function buttonIn(element: Element, text: string) {
  const button = Array.from(element.querySelectorAll("button"))
    .find(value => value.textContent?.trim() === text);
  if (!button) throw new Error(`button not found: ${text}`);
  return button as HTMLButtonElement;
}

function topButton(text: string) {
  const button = Array.from(host!.querySelectorAll("button"))
    .find(value => value.textContent?.trim() === text);
  if (!button) throw new Error(`button not found: ${text}`);
  return button as HTMLButtonElement;
}

async function click(button: HTMLButtonElement) {
  await act(async () => {
    button.click();
    await Promise.resolve();
    await Promise.resolve();
  });
}

function setInputValue(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  if (!setter) throw new Error("input value setter is unavailable");
  setter.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function inputForLabel(labelText: string) {
  const label = Array.from(host!.querySelectorAll("label"))
    .find(element => element.textContent?.includes(labelText));
  const input = label?.querySelector("input");
  if (!input) throw new Error(`input not found for label: ${labelText}`);
  return input as HTMLInputElement;
}

function rosterAccounts(names: { paid?: string; free?: string; unconfigured?: string; incomplete?: string } = {}): Account[] {
  const paidName = names.paid || "Paid Alpha";
  const freeName = names.free || "Free Beta";
  const unconfiguredName = names.unconfigured || "No profile Gamma";
  const incompleteName = names.incomplete || "Incomplete Delta";
  const paidCycle = cycle({ id: 301, account_id: 1, name: paidName });
  const incompleteCycle = cycle({ id: 304, account_id: 4, name: incompleteName, cost_kind: "estimated" });
  return [
    account(paidCycle),
    summarizedAccount(2, freeName, "free", { can_settle: false }),
    summarizedAccount(3, unconfiguredName, "unconfigured", { profile_version: null }),
    summarizedAccount(4, incompleteName, "paid", {
      cycle: incompleteCycle,
      mine: { user_id: 9, user_name: "Admin", consumption: "1.00", amount_fen: 500, share: "0.1", self_covered: false },
      total_consumption: "1.00",
      record_count: 2,
      can_settle: false,
      data_error: "usage records are incomplete",
    }),
  ];
}

function overviewFor(accounts: Account[]): Overview {
  return { ...overview, accounts };
}

function namesFor(accounts: Account[]) {
  return accounts.map(({ id, name }) => ({ id, name }));
}

async function removeAccount(setup: ReturnType<typeof setupFetch>) {
  await act(async () => {
    document.dispatchEvent(new Event("visibilitychange"));
    await Promise.resolve();
  });
  expect(setup.stateCalls()).toBe(2);
  setup.nextState.resolve(response(stateSnapshot([{ id: 2, name: "Active account" }], "2026-10-02T00:00:00.000Z")));
  await flush();
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  Object.defineProperty(document, "hidden", { configurable: true, value: false });
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = undefined;
  host?.remove();
  host = undefined;
  HTMLDialogElement.prototype.showModal = nativeShowModal;
  HTMLDialogElement.prototype.close = nativeClose;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  Reflect.deleteProperty(document, "hidden");
});

describe("App account visibility", () => {
  it("filters removed accounts and clears their selection and edit window", async () => {
    const setup = setupFetch();
    await renderApp();
    await flush();

    const row = rowFor("Removed account");
    const selection = row.querySelector<HTMLInputElement>('input[aria-label="选择 Removed account"]');
    if (!selection) throw new Error("account selection checkbox not found");
    await act(async () => {
      selection.click();
      await Promise.resolve();
    });
    expect(host!.textContent).toContain("结算 (1)");

    await click(buttonIn(row, "修改成本"));
    expect(host!.textContent).toContain("修改周期成本");

    await removeAccount(setup);
    expect(host!.textContent).not.toContain("Removed account");
    expect(host!.textContent).not.toContain("修改周期成本");
    expect(host!.textContent).not.toContain("结算 (1)");
    expect(host!.textContent).toContain("账号已删除，请选择其他账号");
  });

  it("closes an open settlement window after deletion while keeping financial history visible", async () => {
    const setup = setupFetch();
    await renderApp();
    await flush();

    const row = rowFor("Removed account");
    await click(buttonIn(row, "结算"));
    await flush();
    expect(host!.textContent).toContain("结算预览 · 1 项");

    await removeAccount(setup);
    expect(host!.textContent).not.toContain("结算预览 · 1 项");
    expect(host!.textContent).toContain("账号已删除，结算窗口已关闭");

    await click(topButton("我的账单"));
    await flush();
    expect(host!.textContent).toContain("Removed account historical bill");
  });

  it("shows the full account roster to admins and exposes batch settlement only for paid-cycle selection", async () => {
    const accounts = rosterAccounts();
    const setup = setupFetch({
      initialState: stateSnapshot(namesFor(accounts)),
      initialOverview: overviewFor(accounts),
      initialCatalog: catalogFor(accounts),
    });
    await renderApp();
    await flush();

    const paidRow = rowFor("Paid Alpha");
    const freeRow = rowFor("Free Beta");
    const unconfiguredRow = rowFor("No profile Gamma");
    const incompleteRow = rowFor("Incomplete Delta");
    expect(freeRow.querySelector('input[type="checkbox"]')).toBeNull();
    expect(unconfiguredRow.querySelector('input[type="checkbox"]')).toBeNull();
    expect(buttonIn(freeRow, "升级付费")).toBeTruthy();
    expect(buttonIn(unconfiguredRow, "配置")).toBeTruthy();
    expect(incompleteRow.querySelector('[data-label="预计分摊"]')?.textContent?.trim()).toBe("—");

    const toolbarButtons = Array.from(host!.querySelectorAll(".toolbar-actions button"));
    expect(toolbarButtons.some(value => value.textContent?.trim() === "配置账号")).toBe(false);
    expect(toolbarButtons.some(value => value.textContent?.includes("批量结算"))).toBe(false);

    const paidSelection = paidRow.querySelector<HTMLInputElement>('input[type="checkbox"]');
    expect(paidSelection).not.toBeNull();
    expect(paidSelection!.disabled).toBe(false);
    await act(async () => {
      paidSelection!.click();
      await Promise.resolve();
    });
    const batchButton = Array.from(host!.querySelectorAll(".toolbar-actions button"))
      .find(value => value.textContent?.trim().startsWith("批量结算"));
    expect(batchButton?.textContent).toContain("批量结算");
  });

  it("shows only paid accounts to an ordinary user", async () => {
    const accounts = rosterAccounts();
    const setup = setupFetch({
      currentActor: { ...actor, role: "user" },
      initialState: stateSnapshot(namesFor(accounts)),
      initialOverview: overviewFor(accounts),
      initialCatalog: catalogFor(accounts),
    });
    await renderApp();
    await flush();

    const rows = Array.from(host!.querySelectorAll("tbody tr"));
    const rowText = rows.map(value => value.textContent || "").join(" ");
    expect(rowText).toContain("Paid Alpha");
    expect(rowText).toContain("Incomplete Delta");
    expect(rowText).not.toContain("Free Beta");
    expect(rowText).not.toContain("No profile Gamma");
    expect(host!.querySelector('input[type="checkbox"]')).toBeNull();
    expect(Array.from(host!.querySelectorAll("button")).some(value => value.textContent?.trim() === "升级付费")).toBe(false);
  });

  it("keeps an unconfigured account editable while polling a new name without losing its cost draft", async () => {
    vi.useFakeTimers();
    const accounts = rosterAccounts();
    const renamedAccounts = rosterAccounts({ unconfigured: "No profile renamed" });
    const setup = setupFetch({
      initialState: stateSnapshot(namesFor(accounts)),
      stateUpdates: [response(stateSnapshot(namesFor(renamedAccounts), "2026-10-02T00:00:00.000Z"))],
      initialOverview: overviewFor(accounts),
      initialCatalog: catalogFor(accounts),
    });
    await renderApp();
    await flush();

    await click(buttonIn(rowFor("No profile Gamma"), "配置"));
    expect(host!.textContent).toContain("配置账号");
    expect(host!.textContent).not.toContain("公开名称");
    const accountName = inputForLabel("账号");
    expect(accountName.readOnly).toBe(true);
    expect(accountName.value).toBe("No profile Gamma");
    const cost = inputForLabel("成本（元）");
    setInputValue(cost, "66.60");
    expect(cost.value).toBe("66.60");

    await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
    await flush();

    expect(host!.textContent).toContain("配置账号");
    expect(inputForLabel("账号").value).toBe("No profile renamed");
    expect(inputForLabel("账号").readOnly).toBe(true);
    expect(inputForLabel("成本（元）").value).toBe("66.60");
  });

  it("updates the searchable account and open-cycle title from the five-second account poll", async () => {
    vi.useFakeTimers();
    const accounts = rosterAccounts();
    const renamedAccounts = rosterAccounts({ paid: "Paid Alpha renamed" });
    const paidCycle = accounts[0].cycle!;
    const setup = setupFetch({
      initialState: stateSnapshot(namesFor(accounts)),
      stateUpdates: [response(stateSnapshot(namesFor(renamedAccounts), "2026-10-02T00:00:00.000Z"))],
      initialOverview: overviewFor(accounts),
      initialCatalog: catalogFor(accounts),
      previewCycle: paidCycle,
    });
    await renderApp();
    await flush();

    const search = host!.querySelector<HTMLInputElement>('input[aria-label="搜索"]');
    if (!search) throw new Error("account search input not found");
    setInputValue(search, "Paid Alpha renamed");
    expect(Array.from(host!.querySelectorAll("tbody tr"))).toHaveLength(0);

    await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
    await flush();

    const renamedRow = rowFor("Paid Alpha renamed");
    const selection = renamedRow.querySelector<HTMLInputElement>('input[type="checkbox"]');
    if (!selection) throw new Error("renamed paid account selection not found");
    await act(async () => { selection.click(); await Promise.resolve(); });
    const batchButton = Array.from(host!.querySelectorAll(".toolbar-actions button"))
      .find(value => value.textContent?.trim().startsWith("批量结算"));
    if (!batchButton) throw new Error("batch settlement button not found after selection");
    await click(batchButton as HTMLButtonElement);
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    await flush();

    expect(host!.querySelector(".allocation-head strong")?.textContent).toBe("Paid Alpha renamed");
    expect(setup.stateCalls()).toBe(2);
  });

  it("does not let an older overview response replace a newer account name", async () => {
    const accounts = rosterAccounts();
    const renamedAccounts = rosterAccounts({ paid: "Paid Alpha current" });
    const staleOverview = deferred<Response>();
    setupFetch({
      initialState: stateSnapshot(namesFor(accounts)),
      stateUpdates: [response(stateSnapshot(namesFor(renamedAccounts), "2026-10-02T00:00:00.000Z"))],
      initialOverview: overviewFor(accounts),
      overviewUpdates: [staleOverview.promise],
      initialCatalog: catalogFor(accounts),
    });
    await renderApp();
    await flush();

    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
      await Promise.resolve();
    });
    await flush();
    expect(rowFor("Paid Alpha current")).toBeTruthy();

    await act(async () => {
      staleOverview.resolve(response(overviewFor(accounts)));
      await Promise.resolve();
    });
    await flush();
    expect(rowFor("Paid Alpha current")).toBeTruthy();
    expect(Array.from(host!.querySelectorAll("tbody tr")).some(value => value.querySelector(".account-title strong")?.textContent === "Paid Alpha")).toBe(false);
  });

  it("refreshes catalog and overview when the account-state roster gains an account", async () => {
    vi.useFakeTimers();
    const initialAccounts = [rosterAccounts()[0]];
    const addedAccount = summarizedAccount(5, "New account without profile", "unconfigured", { profile_version: null });
    const updatedAccounts = [...initialAccounts, addedAccount];
    const setup = setupFetch({
      initialState: stateSnapshot(namesFor(initialAccounts)),
      stateUpdates: [response(stateSnapshot(namesFor(updatedAccounts), "2026-10-02T00:00:00.000Z"))],
      initialOverview: overviewFor(initialAccounts),
      overviewUpdates: [response(overviewFor(updatedAccounts))],
      initialCatalog: catalogFor(initialAccounts),
      catalogUpdates: [response(catalogFor(updatedAccounts))],
    });
    await renderApp();
    await flush();
    expect(Array.from(host!.querySelectorAll("tbody tr")).some(value => value.textContent?.includes("New account without profile"))).toBe(false);

    await act(async () => { await vi.advanceTimersByTimeAsync(5_000); });
    await flush();

    expect(setup.overviewCalls()).toBeGreaterThanOrEqual(2);
    expect(setup.catalogCalls()).toBeGreaterThanOrEqual(2);
    expect(rowFor("New account without profile")).toBeTruthy();
  });

  it("does not render numeric zero when a cycle detail has start_locked set to zero", async () => {
    const detailCycle = cycle({ start_locked: 0 as unknown as boolean });
    const setup = setupFetch({
      cycleDetail: {
        cycle: detailCycle,
        end: "2026-10-01T00:00:00Z",
        total_consumption: "1.00",
        allocations: [],
        record_count: 2,
      },
    });
    await renderApp();
    await flush();

    const row = rowFor("Removed account");
    const detailButton = row.querySelector<HTMLButtonElement>('button[aria-label="Removed account 周期明细"]');
    if (!detailButton) throw new Error("cycle detail button not found");
    await click(detailButton);

    const dialog = host!.querySelector<HTMLDialogElement>("dialog[open]");
    expect(dialog?.textContent).not.toContain("付费起点已锁定");
    const directText = Array.from(dialog?.querySelector(".form")?.childNodes || [])
      .filter(node => node.nodeType === Node.TEXT_NODE)
      .map(node => node.textContent?.trim())
      .filter(Boolean);
    expect(directText).not.toContain("0");
  });
});
