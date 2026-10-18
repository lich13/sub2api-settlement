// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettleForm } from "./main";
import { VendorBillPanel } from "./Vendors";
import type {
  BillPreview,
  Calculation,
  Catalog,
  Cycle,
  Preview,
  VendorBillPreview,
  VendorPeriod,
} from "./types";

let root: Root | undefined;
let host: HTMLDivElement | undefined;
const nativeShowModal = HTMLDialogElement.prototype.showModal;
const nativeClose = HTMLDialogElement.prototype.close;

async function render(node: ReactNode) {
  if (!root) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  }
  await act(async () => {
    root!.render(node);
  });
}

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function click(value: HTMLButtonElement) {
  await act(async () => {
    value.click();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

function button(text: string) {
  const value = Array.from(host!.querySelectorAll<HTMLButtonElement>("button"))
    .find(element => element.textContent?.trim() === text);
  if (!value) throw new Error(`button not found: ${text}`);
  return value;
}

function optionalButton(text: string) {
  return Array.from(host!.querySelectorAll<HTMLButtonElement>("button"))
    .find(element => element.textContent?.trim() === text);
}

function selectForLabel(text: string) {
  const label = Array.from(host!.querySelectorAll("label"))
    .find(element => element.textContent?.includes(text));
  const select = label?.querySelector<HTMLSelectElement>("select");
  if (!select) throw new Error(`select not found for label: ${text}`);
  return select;
}

function rateInput(name: string) {
  const input = host!.querySelector<HTMLInputElement>(`input[aria-label="${name} 补计倍率"]`);
  if (!input) throw new Error(`rate input not found: ${name}`);
  return input;
}

function checkbox(text: string) {
  const value = Array.from(host!.querySelectorAll<HTMLInputElement>("input[type=checkbox]"))
    .find(input => input.closest("label")?.textContent?.includes(text));
  if (!value) throw new Error(`checkbox not found: ${text}`);
  return value;
}

function setInputValue(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  if (!setter) throw new Error("input value setter is unavailable");
  setter.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function setSelectValue(select: HTMLSelectElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set;
  if (!setter) throw new Error("select value setter is unavailable");
  setter.call(select, value);
  select.dispatchEvent(new Event("change", { bubbles: true }));
}

function setFile(input: HTMLInputElement, name: string, text: string) {
  const file = { name, size: text.length, text: async () => text } as File;
  Object.defineProperty(input, "files", { configurable: true, value: [file] });
}

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

function cycle(overrides: Partial<Cycle> = {}): Cycle {
  return {
    id: 7,
    account_id: 101,
    name: "Synthetic account",
    cost_fen: 10_000,
    creditor_id: 1,
    start: "2026-09-01T00:00:00Z",
    end: null,
    status: "open",
    version: 3,
    settlement_id: null,
    cost_kind: "exact",
    cost_source: "upstream_bill",
    ...overrides,
  };
}

function secondCycle(overrides: Partial<Cycle> = {}): Cycle {
  return cycle({ id: 8, account_id: 102, name: "Second account", ...overrides });
}

function catalog(): Catalog {
  return {
    accounts: [
      { id: 101, name: "Synthetic account", platform: "claude", status: "active", deleted: false, created_at: "2026-01-01T00:00:00Z" },
      { id: 102, name: "Second account", platform: "claude", status: "active", deleted: false, created_at: "2026-01-01T00:00:00Z" },
    ],
    users: [{ id: 1, name: "Fixture Admin" }],
    coverage_from: "2026-01-01T00:00:00Z",
  };
}

function calculation(value: Cycle, overrides: Partial<Calculation> = {}): Calculation {
  return {
    cycle: value,
    end: "2026-10-01T00:00:00Z",
    total_consumption: "2.00",
    allocations: [],
    record_count: 2,
    can_settle: true,
    start_confirmed: true,
    ...overrides,
  };
}

function settlementPreview(value: Cycle, overrides: Partial<Calculation> = {}): Preview {
  return {
    id: "settlement-preview",
    expires_at: new Date(Date.now() + 120_000).toISOString(),
    cycles: [calculation(value, overrides)],
  };
}

function billPreview(overrides: Partial<BillPreview> = {}): BillPreview {
  return {
    id: "bill-preview",
    cost_fen: 10_000,
    total: "100.00",
    matched_amount: "100.00",
    unmatched_amount: "0",
    matched_count: 2,
    unmatched_count: 0,
    outside_count: 0,
    outside_amount: "0",
    local_unmatched_count: 0,
    record_count: 2,
    file_name: "usage.csv",
    file_hash: "fixture-hash",
    end: "2026-10-01T00:00:00Z",
    version: 3,
    allocations: [],
    difference_required: false,
    can_confirm: true,
    ...overrides,
  };
}

function vendorPeriod(overrides: Partial<VendorPeriod> = {}): VendorPeriod {
  return {
    id: 21,
    vendor_id: 5,
    name: "Synthetic provider",
    cost_fen: 20_000,
    creditor_id: 1,
    start: "2026-09-01T00:00:00Z",
    end: null,
    status: "open",
    start_confirmed: true,
    version: 4,
    members: [],
    ...overrides,
  };
}

function vendorPreview(overrides: Partial<VendorBillPreview> = {}): VendorBillPreview {
  return {
    ...billPreview({
      id: "vendor-bill-preview",
      cost_fen: 20_000,
      total: "200.00",
      matched_amount: "200.00",
      matched_count: 2,
      file_name: "usage.csv",
    }),
    bill_total_fen: 20_000,
    accounts: [
      { account_id: 101, cycle_id: 7, name: "Synthetic account", amount_fen: 10_000, total_consumption: "1.00", allocations: [] },
      { account_id: 102, cycle_id: 8, name: "Second account", amount_fen: 10_000, total_consumption: "1.00", allocations: [] },
    ],
    basis: "manual_supplement",
    coverage_warnings: [],
    ...overrides,
  };
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
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
  vi.restoreAllMocks();
});

describe("manual billing for an individual cycle", () => {
  it("previews local-only usage without a CSV and posts its manual basis", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) return response(settlementPreview(cycle()));
      if (path.endsWith("/bill-previews")) {
        return response(billPreview({
          total: null,
          file_name: "",
          manual_basis: { source: "local_only", rates: [{ account_id: 101, rate: "1.5" }], ambiguous_usage_ids: [] },
          manual_summary: {
            source: "local_only",
            amount: "3.00",
            accounts: [{ account_id: 101, rate: "1.5", request_count: 2, excluded_count: 0, local_amount: "2.00", amount: "3.00" }],
          },
          local_unmatched: [],
        }));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm ids={[7]} cycles={[cycle()]} catalog={catalog()} done={vi.fn()} close={vi.fn()} />);
    await advance(0);

    setSelectValue(selectForLabel("账单来源"), "local_only");
    setInputValue(rateInput("Synthetic account"), "1.5");
    await click(button("账单预览"));

    const billCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/bill-previews"));
    const body = JSON.parse(String(billCall?.[1]?.body));
    expect(body).toMatchObject({
      version: 3,
      manual_basis: {
        source: "local_only",
        rates: [{ account_id: 101, rate: "1.5" }],
        ambiguous_usage_ids: [],
      },
    });
    expect(body).not.toHaveProperty("csv");
    expect(body).not.toHaveProperty("file_name");
    expect(host!.textContent).toContain("未提供");
    expect(host!.textContent).toContain("手动补计");
    expect(host!.textContent).toContain("确认计费依据");
  });

  it("keeps the legacy payload free of manual_basis when supplementation is disabled", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const csv = "timestamp,cost\n2026-09-20T00:00:00Z,100\n";
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) return response(settlementPreview(cycle()));
      if (path.endsWith("/bill-previews")) return response(billPreview());
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm ids={[7]} cycles={[cycle()]} catalog={catalog()} done={vi.fn()} close={vi.fn()} />);
    await advance(0);
    expect(checkbox("缺失请求补计").checked).toBe(false);
    const fileInput = host!.querySelector<HTMLInputElement>(".bill-import input[type=file]");
    if (!fileInput) throw new Error("bill CSV input not found");
    setFile(fileInput, "usage.csv", csv);
    await act(async () => {
      fileInput.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    await click(button("账单预览"));

    const billCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/bill-previews"));
    const body = JSON.parse(String(billCall?.[1]?.body));
    expect(body).toMatchObject({ csv, file_name: "usage.csv" });
    expect(body).not.toHaveProperty("manual_basis");
  });

  it("locks settlement while a manual billing draft is unconfirmed and unlocks after confirmation", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) return response(settlementPreview(cycle()));
      if (path.endsWith("/bill-previews")) {
        return response(billPreview({
          manual_basis: { source: "local_only", rates: [{ account_id: 101, rate: "1" }], ambiguous_usage_ids: [] },
          manual_summary: { source: "local_only", amount: "2.00", accounts: [] },
        }));
      }
      if (path.endsWith("/bill-confirm")) return response({});
      if (path.endsWith("/settlements")) return response({});
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm ids={[7]} cycles={[cycle()]} catalog={catalog()} done={vi.fn()} close={vi.fn()} />);
    await advance(0);
    expect(button("确认结算").disabled).toBe(false);

    setSelectValue(selectForLabel("账单来源"), "local_only");
    expect(button("确认结算").disabled).toBe(true);

    setInputValue(rateInput("Synthetic account"), "1");
    await click(button("账单预览"));
    await click(button("确认计费依据"));
    await advance(0);

    expect(fetcher.mock.calls.some(([input]) => String(input).endsWith("/bill-confirm"))).toBe(true);
    expect(button("确认结算").disabled).toBe(false);
    expect(fetcher.mock.calls.some(([input]) => String(input).endsWith("/settlements"))).toBe(false);
  });
});

describe("manual billing for a vendor group", () => {
  const alpha = cycle({ id: 7, account_id: 101, name: "Alpha" });
  const beta = secondCycle({ name: "Beta" });

  it("posts independent rates for each account in a local-only preview", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/bill-previews")) return response(vendorPreview({
        manual_basis: { source: "local_only", rates: [{ account_id: 101, rate: "1.25" }, { account_id: 102, rate: "0.5" }], ambiguous_usage_ids: [] },
        manual_summary: {
          source: "local_only",
          amount: "3.00",
          accounts: [
            { account_id: 101, rate: "1.25", request_count: 2, excluded_count: 0, local_amount: "2.00", amount: "2.50" },
            { account_id: 102, rate: "0.5", request_count: 1, excluded_count: 0, local_amount: "1.00", amount: "0.50" },
          ],
        },
      }));
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorBillPanel
      period={vendorPeriod()}
      cycles={[alpha, beta]}
      end="2026-10-01T00:00:00Z"
      previewKey="stable"
      onConfirmed={vi.fn(async () => {})}
    />);
    setSelectValue(selectForLabel("账单来源"), "local_only");
    setInputValue(rateInput("Alpha"), "1.25");
    setInputValue(rateInput("Beta"), "0.5");
    await click(button("账单预览"));

    const billCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/bill-previews"));
    const body = JSON.parse(String(billCall?.[1]?.body));
    expect(body.manual_basis.source).toBe("local_only");
    expect(body.manual_basis.rates).toEqual(expect.arrayContaining([
      { account_id: 101, rate: "1.25" },
      { account_id: 102, rate: "0.5" },
    ]));
    expect(body).not.toHaveProperty("csv");
    expect(body).not.toHaveProperty("file_name");
  });

  it("invalidates an old confirmation when a rate, source, or ambiguity selection changes", async () => {
    let billCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/bill-previews")) {
        billCalls += 1;
        return response(vendorPreview({
          id: `manual-preview-${billCalls}`,
          manual_basis: { source: "local_only", rates: [{ account_id: 101, rate: billCalls === 1 ? "1" : "2" }], ambiguous_usage_ids: [] },
          manual_summary: { source: "local_only", amount: `${billCalls}.00`, accounts: [] },
          local_unmatched: [{ usage_id: 9001, account_id: 101, user_id: 1, created_at: "2026-09-20T00:00:00Z", model: "fixture-model", local_amount: "1.00", rate: "2", amount: "2.00", reason: "ambiguous", included: false }],
        }));
      }
      if (String(input).endsWith("/bill-confirm")) return response({});
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorBillPanel
      period={vendorPeriod()}
      cycles={[alpha]}
      end="2026-10-01T00:00:00Z"
      previewKey="stable"
      onConfirmed={vi.fn(async () => {})}
    />);
    setSelectValue(selectForLabel("账单来源"), "local_only");
    setInputValue(rateInput("Alpha"), "1");
    await click(button("账单预览"));
    expect(optionalButton("确认分摊依据")?.disabled).toBe(false);

    setInputValue(rateInput("Alpha"), "2");
    expect(optionalButton("确认分摊依据")).toBeUndefined();

    await click(button("账单预览"));
    setSelectValue(selectForLabel("账单来源"), "upstream");
    expect(optionalButton("确认分摊依据")).toBeUndefined();

    setSelectValue(selectForLabel("账单来源"), "local_only");
    await click(button("账单预览"));
    const ambiguous = host!.querySelector<HTMLInputElement>('input[aria-label="补计请求 9001"]');
    if (!ambiguous) throw new Error("ambiguous request checkbox not rendered");
    await act(async () => {
      ambiguous.click();
      await Promise.resolve();
    });
    expect(optionalButton("确认分摊依据")).toBeUndefined();
    expect(fetcher.mock.calls.filter(([input]) => String(input).endsWith("/bill-confirm"))).toHaveLength(0);
  });

  it("keeps an unsubmitted source, rate, and CSV draft through an ordinary refresh", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => response({}));
    vi.stubGlobal("fetch", fetcher);
    const props = {
      period: vendorPeriod(),
      cycles: [alpha],
      end: "2026-10-01T00:00:00Z",
      previewKey: "stable",
      onConfirmed: vi.fn(async () => {}),
    };

    await render(<VendorBillPanel {...props} />);
    await act(async () => {
      checkbox("缺失请求补计").click();
      await Promise.resolve();
    });
    setInputValue(rateInput("Alpha"), "1.75");
    const fileInput = host!.querySelector<HTMLInputElement>(".vendor-bill input[type=file]");
    if (!fileInput) throw new Error("vendor CSV input not found");
    setFile(fileInput, "draft.csv", "timestamp,cost\n2026-09-20T00:00:00Z,100\n");
    await act(async () => {
      fileInput.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    await render(<VendorBillPanel {...props} cycles={[...props.cycles]} period={{ ...props.period }} />);
    expect(checkbox("缺失请求补计").checked).toBe(true);
    expect(rateInput("Alpha").value).toBe("1.75");
    expect(host!.querySelector<HTMLInputElement>(".vendor-bill input[type=file]")?.files?.[0]?.name).toBe("draft.csv");
  });

  it("preserves confirmed ambiguity choices on reopen and an automatic preview refresh", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => String(input).endsWith("/bill-previews") ? response(vendorPreview()) : response({}));
    vi.stubGlobal("fetch", fetcher);
    const props = {
      period: vendorPeriod({ bill_import_id: 4, manual_basis: { source: "upstream", rates: [{ account_id: 101, rate: "0.5" }], ambiguous_usage_ids: [9001] } }),
      cycles: [alpha], end: "2026-10-01T00:00:00Z", previewKey: "first", onConfirmed: vi.fn(async () => {}),
    };
    await render(<VendorBillPanel {...props} />);
    await click(button("沿用已导入账单"));
    let calls = fetcher.mock.calls.filter(([input]) => String(input).endsWith("/bill-previews"));
    expect(JSON.parse(String(calls[0][1]?.body)).manual_basis.ambiguous_usage_ids).toEqual([9001]);
    await render(<VendorBillPanel {...props} previewKey="refreshed" />);
    await click(button("沿用已导入账单"));
    calls = fetcher.mock.calls.filter(([input]) => String(input).endsWith("/bill-previews"));
    expect(JSON.parse(String(calls[1][1]?.body)).manual_basis.ambiguous_usage_ids).toEqual([9001]);
    await render(<VendorBillPanel {...props} end="2026-10-02T00:00:00Z" previewKey="new-window" />);
    await click(button("沿用已导入账单"));
    calls = fetcher.mock.calls.filter(([input]) => String(input).endsWith("/bill-previews"));
    expect(JSON.parse(String(calls[2][1]?.body)).manual_basis.ambiguous_usage_ids).toEqual([]);
  });

  it("ignores stale preview errors and results after a newer manual request", async () => {
    const staleResult = deferred<Response>();
    const staleError = deferred<Response>();
    let billCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/bill-previews")) {
        billCalls += 1;
        if (billCalls === 1) return staleResult.promise;
        if (billCalls === 2) return staleError.promise;
        return response(vendorPreview({ id: "new-preview", manual_summary: { source: "local_only", amount: "2.00", accounts: [] } }));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorBillPanel
      period={vendorPeriod()}
      cycles={[alpha]}
      end="2026-10-01T00:00:00Z"
      previewKey="stable"
      onConfirmed={vi.fn(async () => {})}
    />);
    setSelectValue(selectForLabel("账单来源"), "local_only");
    setInputValue(rateInput("Alpha"), "1");
    await click(button("账单预览"));

    setInputValue(rateInput("Alpha"), "2");
    await act(async () => {
      staleResult.resolve(response(vendorPreview({ id: "stale-preview", manual_summary: { source: "local_only", amount: "1.00", accounts: [] } })));
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(optionalButton("确认分摊依据")).toBeUndefined();

    await click(button("账单预览"));
    setInputValue(rateInput("Alpha"), "3");
    await act(async () => {
      staleError.reject(new Error("stale fixture failure"));
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(host!.textContent).not.toContain("stale fixture failure");

    await click(button("账单预览"));
    expect(host!.textContent).toContain("¥2.00");
    expect(host!.textContent).not.toContain("stale fixture failure");
  });
});
