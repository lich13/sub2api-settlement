// @vitest-environment jsdom

import { act, type ComponentProps, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NettingDetails, NettingList, NettingReceivables } from "./Netting";
import type { Actor, NetBalance, NetPayment, Order } from "./types";

let root: Root | undefined;
let host: HTMLDivElement | undefined;

async function render(node: ReactNode) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(node);
  });
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = undefined;
  host?.remove();
  host = undefined;
  vi.unstubAllGlobals();
});

const actor = (id: number, role: Actor["role"] = "user"): Actor => ({
  id, name: id === 1 ? "Alice" : "Bob", role, csrf: "fixture-csrf",
});

const balance = (overrides: Partial<NetBalance> = {}): NetBalance => ({
  user_a: 1,
  user_b: 2,
  name_a: "Alice",
  name_b: "Bob",
  a_to_b_fen: 1_200,
  b_to_a_fen: 800,
  offset_fen: 800,
  amount_fen: 400,
  payer_id: 1,
  creditor_id: 2,
  order_count: 2,
  digest: "digest-fixture",
  ...overrides,
});

const payment = (overrides: Partial<NetPayment> = {}): NetPayment => ({
  ...balance(),
  id: 42,
  actor_id: 1,
  created_at: "2026-10-01T00:30:00Z",
  ...overrides,
});

const order = (overrides: Partial<Order> = {}): Order => ({
  id: 11,
  settlement_id: 7,
  cycle_id: 3,
  payer_id: 1,
  creditor_id: 2,
  payer_name: "Alice",
  creditor_name: "Bob",
  account_name: "Shared account",
  amount_fen: 1_200,
  status: "paid",
  batch_id: null,
  version: 1,
  start: "2026-09-01T00:00:00Z",
  end: "2026-10-01T00:00:00Z",
  created_at: "2026-10-01T00:30:00Z",
  drift: false,
  ...overrides,
});

function listProps(overrides: Partial<ComponentProps<typeof NettingList>> = {}): ComponentProps<typeof NettingList> {
  return {
    me: actor(1),
    balances: [],
    payments: [],
    orders: [],
    adminView: false,
    filter: "all",
    query: "",
    busy: false,
    onPay: vi.fn(),
    onDetail: vi.fn(),
    onOrder: vi.fn(),
    onLegacyPaid: vi.fn(),
    ...overrides,
  };
}

describe("netting views", () => {
  it("shows only the current user's pending balances and keeps actions within their authority", async () => {
    const payable = balance({ payer_id: 2, creditor_id: 1, amount_fen: 500 });
    const otherUserBalance = balance({ user_b: 3, name_b: "Cara", payer_id: 3, creditor_id: 1 });
    const offsetOnly = balance({ user_a: 2, user_b: 4, name_a: "Bob", name_b: "Drew", payer_id: null, creditor_id: null, amount_fen: 0 });
    const unrelatedOffset = balance({ user_b: 5, name_b: "Eli", payer_id: null, creditor_id: null, amount_fen: 0 });
    const onPay = vi.fn();
    const props = listProps({
      me: actor(2),
      balances: [payable, otherUserBalance, offsetOnly, unrelatedOffset],
      payments: [payment()],
      filter: "pending",
      onPay,
    });

    await render(<NettingList {...props} />);

    expect(host!.querySelectorAll(".netting-bill")).toHaveLength(2);
    expect(host!.textContent).toContain("Alice");
    expect(host!.textContent).not.toContain("Cara");
    expect(host!.textContent).toContain("确认转账");
    expect(host!.textContent).toContain("确认对冲");
    expect(host!.textContent).not.toContain("转账结清");
    expect(host!.textContent).not.toContain("Eli");

    const payButton = Array.from(host!.querySelectorAll("button")).find(button => button.textContent === "确认转账");
    const offsetButton = Array.from(host!.querySelectorAll("button")).find(button => button.textContent === "确认对冲");
    expect(payButton).toBeDefined();
    expect(offsetButton).toBeDefined();
    await act(async () => payButton!.click());
    await act(async () => offsetButton!.click());
    expect(onPay).toHaveBeenNthCalledWith(1, payable);
    expect(onPay).toHaveBeenNthCalledWith(2, offsetOnly);
  });

  it("lets administrators inspect all pairs, honors search and disables payment while busy", async () => {
    const aliceBob = balance({ payer_id: 1, creditor_id: 2 });
    const aliceCara = balance({ user_b: 3, name_b: "Cara", payer_id: 1, creditor_id: 3 });
    const onPay = vi.fn();
    const props = listProps({
      me: actor(9, "admin"),
      balances: [aliceBob, aliceCara],
      adminView: true,
      filter: "pending",
      query: "cara",
      busy: true,
      onPay,
    });

    await render(<NettingList {...props} />);

    expect(host!.querySelectorAll(".netting-bill")).toHaveLength(1);
    expect(host!.textContent).toContain("Alice 与 Cara");
    expect(host!.textContent).not.toContain("Alice 与 Bob");
    const settleButton = host!.querySelector<HTMLButtonElement>(".netting-bill .button.primary");
    expect(settleButton?.textContent).toBe("标记已结清");
    expect(settleButton?.disabled).toBe(true);
    expect(host!.querySelector(".raw-orders summary")?.textContent).toBe("原始订单 · 0 张");
    await act(async () => settleButton?.click());
    expect(onPay).not.toHaveBeenCalled();
  });

  it("shows only positive receivables with a creditor and totals the displayed balances", async () => {
    const balances = [
      balance({ amount_fen: 1_200, payer_id: 1, creditor_id: 2 }),
      balance({ user_b: 3, name_b: "Cara", amount_fen: 300, payer_id: 3, creditor_id: 1 }),
      balance({ user_b: 4, name_b: "Drew", amount_fen: 800, creditor_id: null }),
      balance({ user_b: 5, name_b: "Eli", amount_fen: 0, payer_id: null, creditor_id: null }),
    ];

    await render(<NettingReceivables balances={balances} />);

    expect(host!.querySelector(".receivable-summary")?.textContent).toContain("¥15.00");
    expect(host!.querySelectorAll(".receivable-row")).toHaveLength(2);
    expect(host!.textContent).toContain("Alice");
    expect(host!.textContent).toContain("Cara");
    expect(host!.textContent).not.toContain("Drew");
    expect(host!.textContent).not.toContain("Eli");
    expect(host!.querySelectorAll("button")).toHaveLength(0);
  });

  it("shows both original directions, the offset, the net transfer and source orders", async () => {
    const value = {
      ...balance(),
      id: 42,
      created_at: "2026-10-01T00:30:00Z",
      orders: [
        order({ id: 11, account_name: "Alice's account", amount_fen: 1_200, batch_id: 90 }),
        order({ id: 12, payer_id: 2, payer_name: "Bob", creditor_id: 1, creditor_name: "Alice", account_name: "Bob's account", amount_fen: 800 }),
      ],
    };

    await render(<NettingDetails value={value} />);

    expect(host!.textContent).toContain("Alice 应付 Bob");
    expect(host!.textContent).toContain("Bob 应付 Alice");
    expect(host!.textContent).toContain("相互抵扣");
    expect(host!.textContent).toContain("Alice → Bob");
    expect(host!.textContent).toContain("订单 #11 · 合并单 #90");
    expect(host!.textContent).toContain("订单 #12");
    expect(host!.textContent).toContain("结清时间");
    expect(host!.querySelectorAll(".netting-direction")).toHaveLength(2);
  });

  it("labels a zero-balance detail as requiring no payment and handles empty order directions", async () => {
    const value = {
      ...balance({ amount_fen: 0, payer_id: null, creditor_id: null }),
      orders: [order({ payer_id: 1 })],
    };

    await render(<NettingDetails value={value} />);

    expect(host!.textContent).toContain("无需付款");
    expect(host!.textContent).toContain("无待抵扣订单");
  });
});
