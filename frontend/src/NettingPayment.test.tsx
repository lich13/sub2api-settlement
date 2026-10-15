// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./main";

let root: Root | undefined;
let host: HTMLDivElement | undefined;
const nativeShowModal = HTMLDialogElement.prototype.showModal;
const nativeClose = HTMLDialogElement.prototype.close;

async function render(node: ReactNode) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(node);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function click(label: string) {
  const button = Array.from(host!.querySelectorAll("button"))
    .find(element => element.textContent?.trim() === label);
  if (!button) throw new Error(`button not found: ${label}`);
  await act(async () => {
    button.click();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

function setInputValue(input: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const prototype = input instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
  if (!setter) throw new Error("input value setter is unavailable");
  setter.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function response(data: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
  } as Response;
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.stubGlobal("crypto", { randomUUID: () => "fixture-idempotency-key" });
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = undefined;
  host?.remove();
  host = undefined;
  HTMLDialogElement.prototype.showModal = nativeShowModal;
  HTMLDialogElement.prototype.close = nativeClose;
  vi.unstubAllGlobals();
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
      if (path.endsWith("/overview")) return response({ accounts: [], estimated_fen: null, pending_fen: 500, receivable_fen: 0, submitted_fen: 0, last_sync: "", sync_error: null, coverage_from: "2026-01-01T00:00:00Z" });
      if (path.endsWith("/inbox")) return response({ items: [], unread: 0 });
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

    const note = host!.querySelector<HTMLTextAreaElement>("textarea");
    const reference = host!.querySelector<HTMLInputElement>("input[maxlength='128']");
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
});
