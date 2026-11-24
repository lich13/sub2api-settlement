// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { InboxDialog } from "./DetailDialogs";
import { App } from "./main";
import {
  click as clickElement,
  flush,
  jsonResponse as response,
  renderWithDialog,
  setInputValue,
} from "./test-utils";

const dom = renderWithDialog();

async function render(node: ReactNode) {
  await dom.render(node);
  await flush();
}

async function click(label: string) {
  const button = Array.from(dom.host!.querySelectorAll("button"))
    .find(element => element.textContent?.trim() === label);
  if (!button) throw new Error(`button not found: ${label}`);
  await clickElement(button);
}

beforeEach(() => {
  vi.stubGlobal("crypto", { randomUUID: () => "fixture-idempotency-key" });
});

describe("netting payment confirmation", () => {
  it("collects optional note and reference and sends them with the confirmed payment", async () => {
    const balance = {
      user_a: 2,
      user_b: 3,
      name_a: "Creditor",
      name_b: "Payer",
      a_to_b_fen: 0,
      b_to_a_fen: 500,
      offset_fen: 0,
      amount_fen: 500,
      payer_id: 3,
      creditor_id: 2,
      order_count: 1,
      digest: "payment-digest",
    };
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/me")) return response({ id: 3, name: "Payer", role: "user", csrf: "fixture-csrf" });
      if (path.endsWith("/accounts/state")) return response({ revision: "1", active_ids: [100], checked_at: null, error: null });
      if (path.endsWith("/overview")) return response({ accounts: [], estimated_fen: null, pending_fen: 500, receivable_fen: 0, last_sync: "", sync_error: null, coverage_from: "2026-01-01T00:00:00Z" });
      if (path.endsWith("/inbox")) return response({ items: [], unread: 0 });
      if (path.endsWith("/vendors")) return response({ vendors: [] });
      if (path.endsWith("/expenses")) return response({ expenses: [] });
      if (path.endsWith("/orders?scope=mine")) return response({ orders: [] });
      if (path.endsWith("/netting/balances?scope=mine")) return response({ balances: [balance] });
      if (path.endsWith("/netting/payments?scope=mine")) return response({ payments: [] });
      if (path.endsWith("/netting/payments")) return response({ id: 41 });
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<App />);
    await click("我的账单");
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    await click("确认转账");

    const note = dom.host!.querySelector<HTMLTextAreaElement>("textarea");
    const reference = dom.host!.querySelector<HTMLInputElement>("input[maxlength='128']");
    expect(note?.maxLength).toBe(500);
    expect(reference?.maxLength).toBe(128);
    setInputValue(note!, "十月账单已转账");
    setInputValue(reference!, "BANK-2026-10");
    await click("确认");

    const payment = fetcher.mock.calls.find(([input, init]) =>
      String(input).endsWith("/netting/payments") && init?.method === "POST",
    );
    expect(payment).toBeDefined();
    expect(JSON.parse(String(payment?.[1]?.body))).toMatchObject({
      user_a: 2,
      user_b: 3,
      digest: "payment-digest",
      note: "十月账单已转账",
      reference: "BANK-2026-10",
    });
  });

  it("preserves payment memo draft on failure and resets it after closing and reopening", async () => {
    const balance = {
      user_a: 2,
      user_b: 3,
      name_a: "Creditor",
      name_b: "Payer",
      a_to_b_fen: 0,
      b_to_a_fen: 500,
      offset_fen: 0,
      amount_fen: 500,
      payer_id: 3,
      creditor_id: 2,
      order_count: 1,
      digest: "payment-digest",
    };
    let postAttempts = 0;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = String(input);
      if (path.endsWith("/me")) return response({ id: 3, name: "Payer", role: "user", csrf: "fixture-csrf" });
      if (path.endsWith("/accounts/state")) return response({ revision: "1", active_ids: [100], checked_at: null, error: null });
      if (path.endsWith("/overview")) return response({ accounts: [], estimated_fen: null, pending_fen: 500, receivable_fen: 0, last_sync: "", sync_error: null, coverage_from: "2026-01-01T00:00:00Z" });
      if (path.endsWith("/inbox")) return response({ items: [], unread: 0 });
      if (path.endsWith("/vendors")) return response({ vendors: [] });
      if (path.endsWith("/expenses")) return response({ expenses: [] });
      if (path.endsWith("/orders?scope=mine")) return response({ orders: [] });
      if (path.endsWith("/netting/balances?scope=mine")) return response({ balances: [balance] });
      if (path.endsWith("/netting/payments?scope=mine")) return response({ payments: [] });
      if (path.endsWith("/netting/payments") && init?.method === "POST") {
        postAttempts += 1;
        if (postAttempts === 1) return response({ detail: "fixture payment failed" }, 503);
        return response({ id: 42 });
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<App />);
    await click("我的账单");
    await flush();
    await click("确认转账");

    const note = dom.host!.querySelector<HTMLTextAreaElement>("textarea")!;
    const reference = dom.host!.querySelector<HTMLInputElement>("input[maxlength='128']")!;
    setInputValue(note, "保留草稿备注");
    setInputValue(reference, "REF-KEEP-1");
    await click("确认");
    await flush();

    expect(dom.host!.querySelectorAll('[role="alert"]')).toHaveLength(1);
    expect(dom.host!.querySelector('[role="alert"]')?.textContent).toContain("fixture payment failed");
    expect(dom.host!.querySelector<HTMLTextAreaElement>("textarea")?.value).toBe("保留草稿备注");
    expect(dom.host!.querySelector<HTMLInputElement>("input[maxlength='128']")?.value).toBe("REF-KEEP-1");

    await click("取消");
    await flush();
    expect(dom.host!.querySelector('[role="alert"]')).toBeNull();
    await click("确认转账");

    expect(dom.host!.querySelector('[role="alert"]')).toBeNull();
    expect(dom.host!.querySelector<HTMLTextAreaElement>("textarea")?.value).toBe("");
    expect(dom.host!.querySelector<HTMLInputElement>("input[maxlength='128']")?.value).toBe("");
  });
});

describe("InboxDialog semantics", () => {
  it("renders linked notifications as buttons and orderless notifications as non-interactive divs", async () => {
    const onClose = vi.fn();
    const onOpenOrder = vi.fn();
    await render(
      <InboxDialog
        items={[
          { id: 1, title: "新结算单待付款", order_id: 88, created_at: "2026-10-01T00:00:00Z" },
          { id: 2, title: "对冲转账已确认", order_id: null, created_at: "2026-10-02T00:00:00Z" },
        ]}
        onClose={onClose}
        onOpenOrder={onOpenOrder}
      />,
    );

    const items = Array.from(dom.host!.querySelectorAll<HTMLElement>(".inbox-item"));
    expect(items).toHaveLength(2);
    expect(items[0].tagName).toBe("BUTTON");
    expect(items[0].classList.contains("inbox-link")).toBe(true);
    expect(items[1].tagName).toBe("DIV");
    expect(items[1].classList.contains("inbox-link")).toBe(false);
    expect(items[1].querySelector("svg")).toBeNull();

    await clickElement(items[0]);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onOpenOrder).toHaveBeenCalledWith(88);
  });
});
