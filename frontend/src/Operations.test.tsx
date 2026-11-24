// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AuditLog, OperationStatus } from "./Operations";
import {
  deferred,
  flush,
  jsonResponse as response,
  renderWithDialog,
  setInputValue,
  setSelectValue,
} from "./test-utils";

const dom = renderWithDialog();

async function mount(element: ReactNode) {
  await dom.render(element);
  await flush();
}

function audit(id: number, action: string, marker: string) {
  return {
    id,
    actor_name: "Fixture Admin",
    action,
    entity_id: id * 10,
    created_at: "2026-10-01T00:00:00.000Z",
    payload: { marker },
  };
}

beforeEach(() => {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: "visible",
  });
});

afterEach(() => {
  Reflect.deleteProperty(document, "visibilityState");
});

describe("OperationStatus", () => {
  it("shows pending state, surfaces a failed refresh, and clears the error after recovery", async () => {
    const initial = deferred<Response>();
    let calls = 0;
    const fetcher = vi.fn<typeof fetch>(async () => {
      calls += 1;
      if (calls === 1) return initial.promise;
      if (calls === 2) return response({ detail: "fixture status unavailable" }, 503);
      return response({
        last_sync: "2026-10-02T00:00:00.000Z",
        sync_error: null,
        accounts: { checked_at: "2026-10-02T00:01:00.000Z", error: null },
        pending_deletions: 0,
        checkpoint: null,
      });
    });
    vi.stubGlobal("fetch", fetcher);

    await mount(<OperationStatus />);
    expect(dom.host!.textContent).toContain("尚未同步");
    expect(dom.host!.textContent).toContain("—");

    await act(async () => {
      initial.resolve(response({
        last_sync: "2026-10-01T00:00:00.000Z",
        sync_error: null,
        accounts: { checked_at: "2026-10-01T00:01:00.000Z", error: null },
        pending_deletions: 2,
        checkpoint: { mode: "full", watermark: 4, last_reconciled: "2026-10-01T00:02:00.000Z" },
      }));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(dom.host!.textContent).toContain("2");
    expect(dom.host!.textContent).toContain("全量对账");

    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(dom.host!.querySelector('[role="alert"]')?.textContent).toContain("fixture status unavailable");

    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(dom.host!.querySelector('[role="alert"]')).toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(3);
  });

  it("renders both consumption and account sync errors without dropping either or duplicating identical messages", async () => {
    let calls = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>(async () => {
        calls += 1;
        return response({
          last_sync: null,
          sync_error: "消费同步异常",
          accounts: {
            checked_at: null,
            error: calls === 1 ? "账号同步受阻" : "消费同步异常",
          },
          pending_deletions: 0,
          checkpoint: null,
        });
      }),
    );

    await mount(<OperationStatus />);
    expect(
      Array.from(dom.host!.querySelectorAll('[role="alert"]')).map((el) =>
        el.textContent?.trim(),
      ),
    ).toEqual(["消费同步异常", "账号同步受阻"]);

    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(
      Array.from(dom.host!.querySelectorAll('[role="alert"]')).map((el) =>
        el.textContent?.trim(),
      ),
    ).toEqual(["消费同步异常"]);
  });
});

describe("AuditLog", () => {
  it("ignores a stale page response after a filter starts a newer request", async () => {
    const oldPage = deferred<Response>();
    const fetcher = vi.fn<typeof fetch>(input => {
      const path = String(input);
      if (path.endsWith("/audit?")) return oldPage.promise;
      if (path.includes("/audit?action=settle")) {
        return Promise.resolve(response({ items: [audit(2, "settle", "new result")], next_before: null }));
      }
      return Promise.resolve(response({ items: [], next_before: null }));
    });
    vi.stubGlobal("fetch", fetcher);

    await mount(<AuditLog />);
    expect(fetcher).toHaveBeenCalledTimes(1);

    const select = dom.host!.querySelector<HTMLSelectElement>("select")!;
    setSelectValue(select, "settle");
    await flush();
    expect(dom.host!.textContent).toContain("new result");

    await act(async () => {
      oldPage.resolve(response({ items: [audit(1, "create_cycle", "stale result")], next_before: null }));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(dom.host!.textContent).toContain("new result");
    expect(dom.host!.textContent).not.toContain("stale result");
    expect(dom.host!.querySelector(".audit-list details > summary > svg")).not.toBeNull();
  });

  it("uses Beijing date filters for listing and export, then reports an export error", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.includes("/audit.csv?")) return response({ detail: "fixture export rejected" }, 413);
      return response({ items: [], next_before: null });
    });
    vi.stubGlobal("fetch", fetcher);

    await mount(<AuditLog />);
    await flush();
    expect(dom.host!.querySelector(".audit-list")).toBeNull();
    expect(dom.host!.textContent).toContain("暂无操作记录");

    const select = dom.host!.querySelector<HTMLSelectElement>("select")!;
    setSelectValue(select, "settle");
    const inputs = dom.host!.querySelectorAll<HTMLInputElement>('input[type="datetime-local"]');
    setInputValue(inputs[0], "2026-10-01T00:00");
    setInputValue(inputs[1], "2026-10-02T00:00");
    await flush();

    const filteredPaths = fetcher.mock.calls
      .map(call => String(call[0]))
      .filter(path => path.includes("/audit?action=settle&from="));
    const filteredPath = filteredPaths[filteredPaths.length - 1];
    expect(filteredPath).toBeDefined();
    expect(filteredPath).toContain("from=2026-09-30T16%3A00%3A00.000Z");
    expect(filteredPath).toContain("to=2026-10-01T16%3A00%3A00.000Z");

    const exportButton = Array.from(dom.host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "导出 CSV") as HTMLButtonElement;
    await act(async () => {
      exportButton.click();
      await Promise.resolve();
      await Promise.resolve();
    });
    await flush();

    const exportPath = fetcher.mock.calls
      .map(call => String(call[0]))
      .find(path => path.includes("/audit.csv?"));
    expect(exportPath).toContain("action=settle");
    expect(exportPath).toContain("from=2026-09-30T16%3A00%3A00.000Z");
    expect(exportPath).toContain("to=2026-10-01T16%3A00%3A00.000Z");
    expect(dom.host!.querySelector('[role="alert"]')?.textContent).toContain("fixture export rejected");
  });

  it("hides retired actions from filter options while keeping active maintenance and translating historical audit rows", async () => {
    const retired = [
      ["confirm_local_billing", "确认本地计费依据"],
      ["merge", "合并订单"],
      ["submit", "付款方结清"],
      ["confirm", "管理员标记结清"],
      ["reject", "退回付款"],
      ["split", "拆分订单"],
    ] as const;
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>(async () =>
        response({
          items: [
            audit(1, "reset_estimated", "active-maint"),
            ...retired.map(([key], idx) => audit(idx + 2, key, `hist-${key}`)),
          ],
          next_before: null,
        }),
      ),
    );

    await mount(<AuditLog />);
    const optionValues = Array.from(
      dom.host!.querySelectorAll<HTMLOptionElement>("select option"),
    ).map((opt) => opt.value);
    expect(optionValues).toContain("settle");
    expect(optionValues).toContain("confirm_vendor_bill");
    expect(optionValues).toContain("reset_estimated");
    expect(dom.host!.textContent).toContain("撤销旧待确认成本配置");
    for (const [key, label] of retired) {
      expect(optionValues).not.toContain(key);
      expect(dom.host!.textContent).toContain(label);
    }
  });
});
