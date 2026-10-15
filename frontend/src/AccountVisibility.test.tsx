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

function settlementPreview(): Preview {
  return {
    id: "preview-101",
    expires_at: "2026-10-02T00:00:00Z",
    cycles: [{
      cycle: removedCycle,
      end: "2026-10-01T00:00:00Z",
      total_consumption: "1.00",
      allocations: [],
      record_count: 2,
      can_settle: true,
      start_confirmed: true,
    }],
  };
}

function setupFetch() {
  let stateCalls = 0;
  const nextState = deferred<Response>();
  const fetcher = vi.fn<typeof fetch>(async input => {
    const path = String(input);
    if (path.endsWith("/me")) return response(actor);
    if (path.endsWith("/accounts/state")) {
      stateCalls += 1;
      if (stateCalls === 1) return response({ revision: "2026-10-01T00:00:00.000Z", active_ids: [1, 2], checked_at: "2026-10-01T00:00:00.000Z", error: null });
      return nextState.promise;
    }
    if (path.endsWith("/overview")) return response(overview);
    if (path.endsWith("/catalog")) return response(catalog);
    if (path.endsWith("/inbox")) return response({ items: [], unread: 0 });
    if (path.endsWith("/expenses")) return response({ expenses: [] });
    if (path.endsWith("/previews")) return response(settlementPreview());
    if (path.endsWith("/history")) return response({ cycles: [removedCycle, activeCycle], audit: [] });
    if (path.endsWith("/account-deletions")) return response({ items: [] });
    if (path.endsWith("/orders?scope=mine")) return response({ orders: [historicalOrder] });
    if (path.includes("/netting/balances")) return response({ balances: [] });
    if (path.includes("/netting/payments")) return response({ payments: [] });
    return response({});
  });
  vi.stubGlobal("fetch", fetcher);
  return { fetcher, nextState, stateCalls: () => stateCalls };
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

async function removeAccount(setup: ReturnType<typeof setupFetch>) {
  await act(async () => {
    document.dispatchEvent(new Event("visibilitychange"));
    await Promise.resolve();
  });
  expect(setup.stateCalls()).toBe(2);
  setup.nextState.resolve(response({ revision: "2026-10-02T00:00:00.000Z", active_ids: [2], checked_at: "2026-10-02T00:00:00.000Z", error: null }));
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
});
