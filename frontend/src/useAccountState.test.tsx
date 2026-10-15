// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAccountState } from "./useAccountState";

type HookValue = ReturnType<typeof useAccountState>;

let root: Root | undefined;
let host: HTMLDivElement | undefined;
let value: HookValue | undefined;

function Probe({ userId }: { userId?: number }) {
  value = useAccountState(userId);
  return null;
}

async function mount(userId?: number) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(<Probe userId={userId} />);
  });
}

function current() {
  if (!value) throw new Error("hook has not rendered");
  return value;
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

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  Object.defineProperty(document, "hidden", { configurable: true, value: false });
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  root = undefined;
  host?.remove();
  host = undefined;
  value = undefined;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  Reflect.deleteProperty(document, "hidden");
});

describe("useAccountState", () => {
  it("polls only while visible and keeps the last active IDs when a refresh fails", async () => {
    vi.useFakeTimers();
    let calls = 0;
    const fetcher = vi.fn<typeof fetch>(async () => {
      calls += 1;
      if (calls === 1) {
        return response({ revision: "2026-10-01T00:00:00.000Z", accounts: [{ id: 1, name: "Alpha" }], active_ids: [1], checked_at: "2026-10-01T00:00:00.000Z", error: null });
      }
      if (calls === 2) {
        return response({ revision: "2026-10-02T00:00:00.000Z", accounts: [{ id: 1, name: "Alpha renamed" }, { id: 2, name: "Beta" }], active_ids: [1, 2], checked_at: "2026-10-02T00:00:00.000Z", error: null });
      }
      throw new Error("network unavailable");
    });
    vi.stubGlobal("fetch", fetcher);

    await mount(9);
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(current().state).toMatchObject({
      revision: "2026-10-01T00:00:00.000Z",
      accounts: [{ id: 1, name: "Alpha" }],
      active_ids: [1],
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(current().state).toMatchObject({
      revision: "2026-10-02T00:00:00.000Z",
      accounts: [{ id: 1, name: "Alpha renamed" }, { id: 2, name: "Beta" }],
      active_ids: [1, 2],
    });

    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(fetcher).toHaveBeenCalledTimes(2);

    Object.defineProperty(document, "hidden", { configurable: true, value: false });
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await Promise.resolve();
    });
    expect(fetcher).toHaveBeenCalledTimes(3);
    expect(current().state).toMatchObject({
      revision: "2026-10-02T00:00:00.000Z",
      accounts: [{ id: 1, name: "Alpha renamed" }, { id: 2, name: "Beta" }],
      active_ids: [1, 2],
      error: "账号状态核对失败，暂保留最近列表",
    });
  });

  it("does not let an older out-of-order revision replace the newest state", async () => {
    vi.useFakeTimers();
    const first = deferred<Response>();
    const second = deferred<Response>();
    const fetcher = vi.fn<typeof fetch>()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    vi.stubGlobal("fetch", fetcher);

    await mount(9);
    await act(async () => { await Promise.resolve(); });
    expect(fetcher).toHaveBeenCalledTimes(1);

    const refreshPromise = current().refresh();
    expect(fetcher).toHaveBeenCalledTimes(2);

    await act(async () => {
      second.resolve(response({ revision: "2026-10-02T00:00:00.000Z", accounts: [{ id: 2, name: "Newest" }], active_ids: [2], checked_at: "2026-10-02T00:00:00.000Z", error: null }));
      await refreshPromise;
    });
    expect(current().state).toMatchObject({ revision: "2026-10-02T00:00:00.000Z", accounts: [{ id: 2, name: "Newest" }], active_ids: [2] });

    await act(async () => {
      first.resolve(response({ revision: "2026-10-01T00:00:00.000Z", accounts: [{ id: 1, name: "Older" }], active_ids: [1], checked_at: "2026-10-01T00:00:00.000Z", error: null }));
      await Promise.resolve();
    });
    expect(current().state).toMatchObject({ revision: "2026-10-02T00:00:00.000Z", accounts: [{ id: 2, name: "Newest" }], active_ids: [2] });
  });
});
