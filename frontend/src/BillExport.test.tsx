// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettleForm } from "./main";
import type { BillPreview, Catalog, Calculation, Cycle, Preview } from "./types";
import { setCsrf } from "./api";

let root: Root | undefined;
let host: HTMLDivElement | undefined;
const originalCreateObjectURL = URL.createObjectURL;
const originalRevokeObjectURL = URL.revokeObjectURL;
const nativeShowModal = HTMLDialogElement.prototype.showModal;
const nativeClose = HTMLDialogElement.prototype.close;

async function render(node: ReactNode) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => root!.render(node));
}

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function click(button: HTMLButtonElement) {
  await act(async () => {
    button.click();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function selectBillFile() {
  const input = host!.querySelector<HTMLInputElement>('input[type="file"]');
  if (!input) throw new Error("bill CSV input was not rendered");
  const file = new File(["synthetic bill"], "synthetic.csv", { type: "text/csv" });
  Object.defineProperty(file, "text", { value: async () => "synthetic bill" });
  Object.defineProperty(input, "files", { configurable: true, value: [file] });
  await act(async () => {
    input.dispatchEvent(new Event("change", { bubbles: true }));
    await Promise.resolve();
    await Promise.resolve();
  });
}

function response(data: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
    blob: async () => new Blob(["fixture csv"], { type: "text/csv" }),
  } as Response;
}

const catalog: Catalog = {
  accounts: [],
  users: [{ id: 1, name: "Fixture Admin" }],
  coverage_from: "2025-01-01T00:00:00Z",
};

function cycle(overrides: Partial<Cycle> = {}): Cycle {
  return {
    id: 7,
    account_id: 100,
    name: "Synthetic account",
    cost_fen: 10_000,
    creditor_id: 1,
    start: "2025-01-01T00:00:00Z",
    end: null,
    status: "open",
    version: 1,
    settlement_id: null,
    cost_kind: "estimated",
    ...overrides,
  };
}

function settlementPreview(value: Cycle): Preview {
  const calculation: Calculation = {
    cycle: value,
    end: "2025-02-01T00:00:00Z",
    total_consumption: "1.25",
    allocations: [],
    record_count: 1,
    can_settle: true,
    start_confirmed: true,
  };
  return {
    id: "settlement-preview",
    expires_at: new Date(Date.now() + 120_000).toISOString(),
    cycles: [calculation],
  };
}

const billPreview: BillPreview = {
  id: "bill-preview",
  cost_fen: 125,
  total: "1.25",
  matched_amount: "1.00",
  unmatched_amount: "0.25",
  matched_count: 1,
  unmatched_count: 1,
  outside_count: 1,
  outside_amount: "0.10",
  local_unmatched_count: 0,
  record_count: 2,
  file_name: "synthetic.csv",
  file_hash: "fixture-hash",
  end: "2025-02-01T00:00:00Z",
  version: 1,
  allocations: [],
  difference_required: true,
  can_confirm: true,
};

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
  setCsrf("fixture-csrf", 1);
  Object.defineProperty(URL, "createObjectURL", {
    configurable: true,
    value: vi.fn(() => "blob:fixture-export"),
  });
  Object.defineProperty(URL, "revokeObjectURL", {
    configurable: true,
    value: vi.fn(),
  });
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = undefined;
  host?.remove();
  host = undefined;
  if (originalCreateObjectURL) {
    Object.defineProperty(URL, "createObjectURL", { configurable: true, value: originalCreateObjectURL });
  } else {
    delete (URL as unknown as { createObjectURL?: typeof URL.createObjectURL }).createObjectURL;
  }
  if (originalRevokeObjectURL) {
    Object.defineProperty(URL, "revokeObjectURL", { configurable: true, value: originalRevokeObjectURL });
  } else {
    delete (URL as unknown as { revokeObjectURL?: typeof URL.revokeObjectURL }).revokeObjectURL;
  }
  HTMLDialogElement.prototype.showModal = nativeShowModal;
  HTMLDialogElement.prototype.close = nativeClose;
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  setCsrf("");
});

describe("unmatched bill export", () => {
  it("shows the export when unmatched rows exist and downloads the preview CSV", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) return response(settlementPreview(cycle()));
      if (path.endsWith("/bill-previews")) return response(billPreview);
      if (path.endsWith("/unmatched.csv")) return response(undefined);
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm ids={[7]} cycles={[cycle()]} catalog={catalog} done={vi.fn()} close={vi.fn()} />);
    await advance(0);
    await selectBillFile();

    const previewButton = Array.from(host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "账单预览");
    expect(previewButton).toBeDefined();
    await click(previewButton!);

    const exportButton = Array.from(host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "导出未匹配明细");
    expect(exportButton).toBeDefined();
    await click(exportButton!);

    expect(fetcher.mock.calls.some(([input]) =>
      String(input).endsWith("/cycles/7/bill-previews/bill-preview/unmatched.csv"),
    )).toBe(true);
    expect(URL.createObjectURL).toHaveBeenCalledOnce();
    expect(HTMLAnchorElement.prototype.click).toHaveBeenCalledOnce();
    expect(host!.textContent).not.toContain("导出失败");
  });

  it("shows the export error when the download endpoint rejects the request", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) return response(settlementPreview(cycle()));
      if (path.endsWith("/bill-previews")) return response(billPreview);
      if (path.endsWith("/unmatched.csv")) return response({ detail: "预览已过期" }, 409);
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm ids={[7]} cycles={[cycle()]} catalog={catalog} done={vi.fn()} close={vi.fn()} />);
    await advance(0);
    await selectBillFile();
    const previewButton = Array.from(host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "账单预览");
    await click(previewButton!);
    const exportButton = Array.from(host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "导出未匹配明细");
    expect(exportButton).toBeDefined();
    await click(exportButton!);

    expect(host!.textContent).toContain("预览已过期");
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });
});
