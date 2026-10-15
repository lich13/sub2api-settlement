// @vitest-environment jsdom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useAutoPreview } from "./useAutoPreview";
import type { Preview } from "./types";

type HookInput = Parameters<typeof useAutoPreview>[0];
type HookValue = ReturnType<typeof useAutoPreview>;

let root: Root | undefined;
let host: HTMLDivElement | undefined;
let value: HookValue | undefined;

function Probe(props: HookInput) {
  value = useAutoPreview(props);
  return null;
}

async function mount(input: HookInput) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(<Probe {...input} />);
  });
}

async function rerender(input: HookInput) {
  await act(async () => {
    root!.render(<Probe {...input} />);
  });
}

function current() {
  if (!value) throw new Error("hook has not rendered");
  return value;
}

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

function response(data: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
  } as Response;
}

function preview(id: string, lifetimeMs = 60_000): Preview {
  return {
    id,
    expires_at: new Date(Date.now() + lifetimeMs).toISOString(),
    cycles: [],
  };
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
});

afterEach(async () => {
  if (root) {
    await act(async () => root!.unmount());
  }
  root = undefined;
  host?.remove();
  host = undefined;
  value = undefined;
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("useAutoPreview", () => {
  it("previews on open with the Beijing cutoff and calls the current result callback", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-30T00:00:00.000Z"));
    const result = preview("preview-1");
    const fetcher = vi.fn().mockResolvedValue(response(result));
    vi.stubGlobal("fetch", fetcher);
    const initialCallback = vi.fn();
    const latestCallback = vi.fn();
    const input: HookInput = {
      ids: [4, 9],
      expenseIds: [12],
      end: "2026-10-01T00:30",
      revision: "r1",
      onResult: initialCallback,
    };

    await mount(input);
    expect(current().loading).toBe(true);
    await advance(0);

    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(fetcher.mock.calls[0][0]).toBe("/settlement/api/v1/previews");
    expect(fetcher.mock.calls[0][1]?.method).toBe("POST");
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual({
      ids: [4, 9],
      expense_ids: [12],
      end: "2026-09-30T16:30:00.000Z",
    });
    expect(current()).toMatchObject({ preview: result, loading: false, error: "" });
    expect(initialCallback).toHaveBeenCalledWith(result);

    // A fresh callback can retain the result in caller state without restarting the request.
    await rerender({ ...input, onResult: latestCallback });
    expect(fetcher).toHaveBeenCalledTimes(1);
    await act(async () => current().refresh());
    await advance(0);
    expect(latestCallback).toHaveBeenCalledWith(result);
  });

  it("debounces a changed end time and ignores a response for the previous inputs", async () => {
    vi.useFakeTimers();
    const oldResponse = deferred<Response>();
    const newResponse = deferred<Response>();
    const fetcher = vi.fn()
      .mockReturnValueOnce(oldResponse.promise)
      .mockReturnValueOnce(newResponse.promise);
    vi.stubGlobal("fetch", fetcher);
    const onResult = vi.fn();
    const first: HookInput = {
      ids: [1], expenseIds: [], end: "2026-10-01T10:00", revision: "r1", onResult,
    };
    await mount(first);
    await advance(0);
    expect(fetcher).toHaveBeenCalledTimes(1);

    const changed = { ...first, end: "2026-10-02T10:00" };
    await rerender(changed);
    expect(current()).toMatchObject({ preview: null, loading: true });
    await advance(299);
    expect(fetcher).toHaveBeenCalledTimes(1);
    await advance(1);
    expect(fetcher).toHaveBeenCalledTimes(2);

    await act(async () => {
      oldResponse.resolve(response(preview("stale")));
    });
    expect(current().preview).toBeNull();
    expect(onResult).not.toHaveBeenCalled();

    const fresh = preview("fresh");
    await act(async () => {
      newResponse.resolve(response(fresh));
    });
    expect(current()).toMatchObject({ preview: fresh, loading: false });
    expect(onResult).toHaveBeenCalledTimes(1);
    expect(onResult).toHaveBeenCalledWith(fresh);
  });

  it("recomputes on revision changes and retries after a request error", async () => {
    vi.useFakeTimers();
    const success = preview("after-retry");
    const fetcher = vi.fn()
      .mockRejectedValueOnce(new Error("temporary failure"))
      .mockResolvedValueOnce(response(success))
      .mockResolvedValueOnce(response(preview("new-revision")));
    vi.stubGlobal("fetch", fetcher);
    const input: HookInput = {
      ids: [2], expenseIds: [3], end: "2026-10-01T12:00", revision: "r1",
    };

    await mount(input);
    await advance(0);
    expect(current()).toMatchObject({ preview: null, loading: false, error: "temporary failure" });

    await act(async () => current().refresh());
    await advance(0);
    expect(current()).toMatchObject({ preview: success, loading: false, error: "" });
    expect(fetcher).toHaveBeenCalledTimes(2);

    await rerender({ ...input, revision: "r2" });
    await advance(0);
    expect(fetcher).toHaveBeenCalledTimes(3);
    expect(JSON.parse(String(fetcher.mock.calls[2][1]?.body))).toEqual(
      JSON.parse(String(fetcher.mock.calls[1][1]?.body)),
    );
    expect(current().preview?.id).toBe("new-revision");
  });

  it("refreshes when a preview expires using the same cutoff", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const fetcher = vi.fn()
      .mockResolvedValueOnce(response(preview("short-lived", 1_000)))
      .mockResolvedValueOnce(response(preview("renewed")));
    vi.stubGlobal("fetch", fetcher);
    const input: HookInput = {
      ids: [5], expenseIds: [6], end: "2026-10-01T09:00", revision: "r1",
    };

    await mount(input);
    await advance(0);
    await advance(999);
    expect(fetcher).toHaveBeenCalledTimes(1);
    await advance(1);
    await advance(0);

    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual(
      JSON.parse(String(fetcher.mock.calls[1][1]?.body)),
    );
    expect(current().preview?.id).toBe("renewed");
  });

  it("does not calculate while paused and uses the latest inputs after resuming", async () => {
    vi.useFakeTimers();
    const result = preview("resumed");
    const fetcher = vi.fn().mockResolvedValue(response(result));
    vi.stubGlobal("fetch", fetcher);
    const input: HookInput = {
      ids: [1], expenseIds: [], end: "2026-10-01T10:00", revision: "r1", paused: true,
    };

    await mount(input);
    await advance(1_000);
    await rerender({ ...input, ids: [8], end: "2026-10-02T10:00", revision: "r2" });
    await advance(1_000);
    expect(fetcher).not.toHaveBeenCalled();

    await rerender({ ...input, ids: [8], end: "2026-10-02T10:00", revision: "r2", paused: false });
    await advance(0);
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual({
      ids: [8],
      expense_ids: [],
      end: "2026-10-02T02:00:00.000Z",
    });
  });
});
