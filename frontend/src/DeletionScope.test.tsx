// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";
import { DeletionScope, type DeletionScopeValue } from "./DeletionScope";
import { flush, jsonResponse as response, renderWithDialog } from "./test-utils";

const dom = renderWithDialog();
const render = async (node: ReactNode) => {
  await dom.render(node);
  await flush();
};

describe("DeletionScope", () => {
  it("shows verified root and shadow accounts inline and binds the scope digest without an inner checkbox", async () => {
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
      targets: [
        ...scope.targets,
        { id: 103, name: "新增子账号 Delta", shadow: true },
      ],
    };
    let calls = 0;
    const fetcher = vi.fn<typeof fetch>(async () =>
      response(calls++ === 0 ? scope : updatedScope),
    );
    vi.stubGlobal("fetch", fetcher);
    const onConfirmed = vi.fn();

    await render(<DeletionScope accountId={100} onConfirmed={onConfirmed} />);

    expect(fetcher).toHaveBeenCalledWith(
      "/settlement/api/v1/accounts/100/deletion-scope",
      expect.objectContaining({ method: "GET" }),
    );
    expect(dom.host!.textContent).toContain("母账号 Alpha");
    expect(dom.host!.textContent).toContain("子账号 Beta");
    expect(dom.host!.textContent).toContain("子账号 Gamma");
    expect(dom.host!.textContent).toContain("母账号");
    expect(dom.host!.textContent).toContain("影子账号");
    expect(dom.host!.querySelector("input[type=checkbox]")).toBeNull();
    expect(onConfirmed).toHaveBeenLastCalledWith(scope);

    const recheck = Array.from(dom.host!.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "重新核验范围",
    );
    expect(recheck).toBeDefined();
    await act(async () => {
      recheck!.click();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(dom.host!.textContent).toContain("新增子账号 Delta");
    expect(onConfirmed).toHaveBeenLastCalledWith(updatedScope);
  });
});
