// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DeletionScope, type DeletionScopeValue } from "./DeletionScope";

let root: Root | undefined;
let host: HTMLDivElement | undefined;

async function render(node: ReactNode) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root!.render(node);
    await Promise.resolve();
    await Promise.resolve();
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

function response(data: unknown): Response {
  return { ok: true, status: 200, json: async () => data } as Response;
}

describe("DeletionScope", () => {
  it("shows the verified root and shadow accounts and reports only explicit confirmation", async () => {
    const scope: DeletionScopeValue = {
      account_id: 100,
      digest: "a".repeat(64),
      targets: [
        { id: 100, name: "母账号 Alpha", shadow: false },
        { id: 101, name: "子账号 Beta", shadow: true },
        { id: 102, name: "子账号 Gamma", shadow: true },
      ],
    };
    const updatedScope: DeletionScopeValue = {
      ...scope,
      digest: "b".repeat(64),
      targets: [...scope.targets, { id: 103, name: "新增子账号 Delta", shadow: true }],
    };
    let calls = 0;
    const fetcher = vi.fn<typeof fetch>(async () => response(calls++ === 0 ? scope : updatedScope));
    vi.stubGlobal("fetch", fetcher);
    const onConfirmed = vi.fn();

    await render(<DeletionScope accountId={100} onConfirmed={onConfirmed} />);

    expect(fetcher).toHaveBeenCalledWith(
      "/settlement/api/v1/accounts/100/deletion-scope",
      expect.objectContaining({ method: "GET" }),
    );
    expect(host!.textContent).toContain("母账号 Alpha");
    expect(host!.textContent).toContain("子账号 Beta");
    expect(host!.textContent).toContain("子账号 Gamma");
    expect(host!.textContent).toContain("母账号");
    expect(host!.textContent).toContain("影子账号");

    const checkbox = host!.querySelector<HTMLInputElement>("input[type=checkbox]");
    expect(checkbox).toBeDefined();
    expect(checkbox!.checked).toBe(false);
    expect(onConfirmed).not.toHaveBeenCalledWith(scope);

    await act(async () => {
      checkbox!.click();
      await Promise.resolve();
    });
    expect(onConfirmed).toHaveBeenLastCalledWith(scope);

    await act(async () => {
      checkbox!.click();
      await Promise.resolve();
    });
    expect(onConfirmed).toHaveBeenLastCalledWith(null);

    await act(async () => {
      checkbox!.click();
      await Promise.resolve();
    });
    expect(onConfirmed).toHaveBeenLastCalledWith(scope);
    const recheck = Array.from(host!.querySelectorAll("button"))
      .find(button => button.textContent?.trim() === "重新核验范围");
    expect(recheck).toBeDefined();
    await act(async () => {
      recheck!.click();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(host!.textContent).toContain("新增子账号 Delta");
    expect(checkbox!.checked).toBe(false);
    expect(onConfirmed).toHaveBeenLastCalledWith(null);

    await act(async () => {
      checkbox!.click();
      await Promise.resolve();
    });
    expect(onConfirmed).toHaveBeenLastCalledWith(updatedScope);
  });
});
