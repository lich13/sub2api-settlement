// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AuditLog, OperationStatus } from "./Operations";

let root: Root | undefined;
let host: HTMLDivElement | undefined;

function response(data: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
    blob: async () => new Blob(),
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

async function mount(element: ReactNode) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(element);
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

function setDateInput(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  setter?.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
  input.dispatchEvent(new Event("change", { bubbles: true }));
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: "visible",
  });
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = undefined;
  host?.remove();
  host = undefined;
  vi.unstubAllGlobals();
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
    expect(host!.textContent).toContain("尚未同步");
    expect(host!.textContent).toContain("—");

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
    expect(host!.textContent).toContain("2");
    expect(host!.textContent).toContain("全量对账");

    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(host!.querySelector('[role="alert"]')?.textContent).toContain("fixture status unavailable");

    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(host!.querySelector('[role="alert"]')).toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(3);
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

    const select = host!.querySelector<HTMLSelectElement>("select")!;
    await act(async () => {
      select.value = "settle";
      select.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
      await Promise.resolve();
    });
    await flush();
    expect(host!.textContent).toContain("new result");

    await act(async () => {
      oldPage.resolve(response({ items: [audit(1, "create_cycle", "stale result")], next_before: null }));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(host!.textContent).toContain("new result");
    expect(host!.textContent).not.toContain("stale result");
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

    await act(async () => {
      const select = host!.querySelector<HTMLSelectElement>("select")!;
      select.value = "settle";
      select.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
    });
    await act(async () => {
      const input = host!.querySelectorAll<HTMLInputElement>('input[type="datetime-local"]')[0];
      setDateInput(input, "2026-10-01T00:00");
      await Promise.resolve();
    });
    await act(async () => {
      const input = host!.querySelectorAll<HTMLInputElement>('input[type="datetime-local"]')[1];
      setDateInput(input, "2026-10-02T00:00");
      await Promise.resolve();
    });
    await flush();

    const filteredPaths = fetcher.mock.calls
      .map(call => String(call[0]))
      .filter(path => path.includes("/audit?action=settle&from="));
    const filteredPath = filteredPaths[filteredPaths.length - 1];
    expect(filteredPath).toBeDefined();
    expect(filteredPath).toContain("from=2026-09-30T16%3A00%3A00.000Z");
    expect(filteredPath).toContain("to=2026-10-01T16%3A00%3A00.000Z");

    const exportButton = Array.from(host!.querySelectorAll("button"))
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
    expect(host!.querySelector('[role="alert"]')?.textContent).toContain("fixture export rejected");
  });
});
