// @vitest-environment jsdom

import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettleForm } from "./SettleForm";
import type { BillPreview, Catalog, Calculation, Cycle, Preview } from "./types";
import { setCsrf } from "./api";
import {
  advance,
  click,
  jsonResponse,
  renderWithDialog,
} from "./test-utils";

const dom = renderWithDialog();
const render = dom.render;
const originalCreateObjectURL = URL.createObjectURL;
const originalRevokeObjectURL = URL.revokeObjectURL;

async function selectBillFile() {
  const input = dom.host!.querySelector<HTMLInputElement>('input[type="file"]');
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

const response = (data: unknown, status = 200) => jsonResponse(data, status, "fixture csv");

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
  end: "2025-02-01T00:00:00Z",
  version: 1,
  allocations: [],
  can_confirm: true,
};

beforeEach(() => {
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

afterEach(() => {
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

    const previewButton = Array.from(dom.host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "账单预览");
    expect(previewButton).toBeDefined();
    await click(previewButton!);

    const exportButton = Array.from(dom.host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "导出未匹配明细");
    expect(exportButton).toBeDefined();
    await click(exportButton!);

    expect(fetcher.mock.calls.some(([input]) =>
      String(input).endsWith("/cycles/7/bill-previews/bill-preview/unmatched.csv"),
    )).toBe(true);
    expect(URL.createObjectURL).toHaveBeenCalledOnce();
    expect(HTMLAnchorElement.prototype.click).toHaveBeenCalledOnce();
    expect(dom.host!.textContent).not.toContain("导出失败");
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
    const previewButton = Array.from(dom.host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "账单预览");
    await click(previewButton!);
    const exportButton = Array.from(dom.host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "导出未匹配明细");
    expect(exportButton).toBeDefined();
    await click(exportButton!);

    expect(dom.host!.textContent).toContain("预览已过期");
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });
});
