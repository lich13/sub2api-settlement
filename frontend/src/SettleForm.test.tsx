// @vitest-environment jsdom

import { act, type ComponentProps, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettleForm } from "./main";
import type { Calculation, Catalog, Cycle, Preview, VendorPeriod } from "./types";

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

async function click(button: HTMLButtonElement) {
  await act(async () => {
    button.click();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

function button(text: string) {
  const found = Array.from(host!.querySelectorAll("button"))
    .find(element => element.textContent?.trim() === text);
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

function inputForLabel(labelText: string) {
  const label = Array.from(host!.querySelectorAll("label"))
    .find(element => element.textContent?.includes(labelText));
  const input = label?.querySelector("input");
  if (!input) throw new Error(`input not found for label: ${labelText}`);
  return input as HTMLInputElement;
}

function selectForLabel(labelText: string) {
  const label = Array.from(host!.querySelectorAll("label"))
    .find(element => element.textContent?.includes(labelText));
  const select = label?.querySelector<HTMLSelectElement>("select");
  if (!select) throw new Error(`select not found for label: ${labelText}`);
  return select;
}

function response(data: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
  } as Response;
}

const catalog: Catalog = {
  accounts: [],
  users: [{ id: 1, name: "Alice" }, { id: 2, name: "Bob" }],
  coverage_from: "2026-01-01T00:00:00Z",
};

function cycle(overrides: Partial<Cycle> = {}): Cycle {
  return {
    id: 7,
    account_id: 1,
    name: "Shared account",
    cost_fen: 5_000,
    creditor_id: 1,
    start: "2026-09-01T00:00:00Z",
    end: null,
    status: "open",
    version: 1,
    settlement_id: null,
    cost_kind: "exact",
    ...overrides,
  };
}

function groupCycle(id: number, accountId: number, overrides: Partial<Cycle> = {}): Cycle {
  return cycle({
    id,
    account_id: accountId,
    name: `Group account ${accountId}`,
    cost_fen: 12_345,
    vendor_id: 9,
    vendor_period_id: 55,
    ...overrides,
  });
}

function groupPeriod(members: Cycle[], overrides: Partial<VendorPeriod> = {}): VendorPeriod {
  return {
    id: 55,
    vendor_id: 9,
    name: "Shared provider",
    cost_fen: 12_345,
    creditor_id: 1,
    start: "2026-09-01T00:00:00Z",
    end: null,
    status: "open",
    start_confirmed: true,
    version: 4,
    members,
    ...overrides,
  };
}

function groupPreview(period: VendorPeriod, members: Cycle[], id: string): Preview {
  return {
    id,
    expires_at: new Date(Date.now() + 120_000).toISOString(),
    cycles: members.map(member => calculation(member, { vendor_period: period })),
  };
}

function calculation(value: Cycle, overrides: Partial<Calculation> = {}): Calculation {
  return {
    cycle: value,
    end: "2026-10-01T00:00:00Z",
    total_consumption: "1.25",
    allocations: [],
    record_count: 3,
    can_settle: true,
    start_confirmed: true,
    ...overrides,
  };
}

function preview(value: Cycle, id: string): Preview {
  return {
    id,
    expires_at: new Date(Date.now() + 120_000).toISOString(),
    cycles: [calculation(value)],
  };
}

function formProps(overrides: Partial<ComponentProps<typeof SettleForm>> = {}): ComponentProps<typeof SettleForm> {
  return {
    ids: [7],
    cycles: [cycle()],
    catalog,
    done: vi.fn(),
    close: vi.fn(),
    ...overrides,
  };
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

function checkbox(labelText: string) {
  const found = Array.from(host!.querySelectorAll<HTMLInputElement>("input[type=checkbox]"))
    .find(input => input.closest("label")?.textContent?.includes(labelText));
  if (!found) throw new Error(`checkbox not found: ${labelText}`);
  return found;
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
});

describe("SettleForm preview integration", () => {
  it("keeps an edited renewal draft when a newer preview arrives", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    let previewCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/previews")) {
        previewCalls += 1;
        const version = previewCalls;
        return response(preview(cycle({ version, cost_fen: version === 1 ? 7_100 : 7_500 }), `preview-${version}`));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);
    const props = formProps({ renewByDefault: true });

    await render(<SettleForm {...props} />);
    await advance(0);

    expect(previewCalls).toBe(1);
    const renewalCost = Array.from(host!.querySelectorAll<HTMLInputElement>("input"))
      .find(input => input.closest("label")?.textContent?.includes("新周期成本（元）"));
    expect(renewalCost?.value).toBe("71.00");
    expect(Array.from(host!.querySelectorAll<HTMLInputElement>("input[type=checkbox]"))
      .some(input => input.checked && input.closest("label")?.textContent?.includes("结算后续期"))).toBe(true);

    setInputValue(renewalCost!, "66.60");
    expect(renewalCost!.value).toBe("66.60");
    await render(<SettleForm {...props} cycles={[cycle({ version: 2 })]} />);
    await advance(0);

    expect(previewCalls).toBe(2);
    const updatedRenewalCost = Array.from(host!.querySelectorAll<HTMLInputElement>("input"))
      .find(input => input.closest("label")?.textContent?.includes("新周期成本（元）"));
    expect(updatedRenewalCost?.value).toBe("66.60");
  });

  it("uses one group renewal control for two members and preserves the edited group draft", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const initialMembers = [groupCycle(101, 1001), groupCycle(102, 1002)];
    const initialPeriod = groupPeriod(initialMembers);
    let previewCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/previews")) {
        previewCalls += 1;
        const members = initialMembers.map(member => ({ ...member, version: previewCalls }));
        const period = groupPeriod(members, {
          cost_fen: previewCalls === 1 ? 12_345 : 23_456,
          version: initialPeriod.version + previewCalls,
          members,
        });
        return response(groupPreview(period, members, `group-preview-${previewCalls}`));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);
    const props = formProps({
      ids: initialMembers.map(member => member.id),
      cycles: initialMembers,
      vendorPeriods: [initialPeriod],
      renewByDefault: true,
    });

    await render(<SettleForm {...props} />);
    await advance(0);

    expect(previewCalls).toBe(1);
    const groupRenewalLabels = Array.from(host!.querySelectorAll("label"))
      .filter(label => label.textContent?.trim() === "结算后整组续费");
    expect(groupRenewalLabels).toHaveLength(1);
    expect((groupRenewalLabels[0].querySelector("input") as HTMLInputElement).checked).toBe(true);
    expect(Array.from(host!.querySelectorAll("label"))
      .filter(label => label.textContent?.trim() === "结算后续期")).toHaveLength(0);
    expect(inputForLabel("下期组成本（元）").value).toBe("123.45");

    setInputValue(inputForLabel("下期组成本（元）"), "99.99");
    const updatedMembers = initialMembers.map(member => ({ ...member, version: 2 }));
    const updatedPeriod = groupPeriod(updatedMembers, { version: initialPeriod.version + 1 });
    await render(<SettleForm {...props} cycles={updatedMembers} vendorPeriods={[updatedPeriod]} />);
    await advance(0);

    expect(previewCalls).toBe(2);
    expect(inputForLabel("下期组成本（元）").value).toBe("99.99");
  });

  it("submits one vendor renewal for the whole group without member cycle ids", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const members = [groupCycle(201, 2001, { vendor_period_id: 66 }), groupCycle(202, 2002, { vendor_period_id: 66 })];
    const period = groupPeriod(members, { id: 66, cost_fen: 23_456, version: 8 });
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) return response(groupPreview(period, members, "group-settle-preview"));
      if (path.endsWith("/settlements")) return response({});
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm {...formProps({
      ids: members.map(member => member.id),
      cycles: members,
      vendorPeriods: [period],
    })} />);
    await advance(0);
    const groupRenewal = checkbox("结算后整组续费");
    expect(groupRenewal.checked).toBe(false);
    await act(async () => {
      groupRenewal.click();
      await Promise.resolve();
    });
    expect(groupRenewal.checked).toBe(true);

    await click(button("确认结算"));
    const settlementCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/settlements"));
    const payload = JSON.parse(String(settlementCall?.[1]?.body));
    expect(payload).toMatchObject({
      preview_id: "group-settle-preview",
      renewals: [],
      vendor_renewals: [{ period_id: 66, version: 8, cost: "234.56", creditor_id: 1 }],
    });
    expect(payload).not.toHaveProperty("cycle_ids");
    expect(payload.vendor_renewals[0]).not.toHaveProperty("cycle_ids");
  });

  it("blocks an empty group renewal when every member is deleted", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const members = [
      groupCycle(301, 3001, { account_deleted: true, vendor_period_id: 77 }),
      groupCycle(302, 3002, { account_deleted: true, vendor_period_id: 77 }),
    ];
    const period = groupPeriod(members, { id: 77, version: 9 });
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/previews")) return response(groupPreview(period, members, "group-deleted-preview"));
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm {...formProps({
      ids: members.map(member => member.id),
      cycles: members,
      vendorPeriods: [period],
      renewByDefault: true,
    })} />);
    await advance(0);
    expect(checkbox("结算后整组续费").checked).toBe(true);

    await click(button("确认结算"));
    expect(fetcher.mock.calls.some(([input]) => String(input).endsWith("/settlements"))).toBe(false);
    expect(host!.textContent).toContain("没有可续费成员，请取消整组续费");
  });

  it("keeps renewal and bill-import drafts when the open cycle is renamed", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const cycleValue = cycle({ cost_source: "upstream_bill", bill_import_id: 33, cost_kind: "estimated" });
    const csv = "timestamp,cost\n2026-09-20T00:00:00Z,1.25\n";
    let previewCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) {
        previewCalls += 1;
        return response(preview(cycle({ ...cycleValue, version: previewCalls }), `rename-preview-${previewCalls}`));
      }
      if (path.endsWith("/bill-previews")) return response({
        allocation_mode: "proportional",
        id: "bill-draft-preview",
        cost_fen: 5_000,
        billable_cost_fen: 5_000,
        total: "50.00",
        matched_amount: "50.00",
        unmatched_amount: "0",
        matched_count: 1,
        unmatched_count: 0,
        outside_count: 0,
        outside_amount: "0",
        local_unmatched_count: 0,
        record_count: 1,
        file_name: "draft.csv",
        file_hash: "fixture-hash",
        end: "2026-10-01T00:00:00Z",
        version: 1,
        allocations: [],
        difference_required: false,
        can_confirm: true,
      });
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);
    const initialProps = formProps({ cycles: [cycleValue], renewByDefault: true });

    await render(<SettleForm {...initialProps} />);
    await advance(0);
    const renewal = inputForLabel("新周期成本（元）");
    setInputValue(renewal, "66.60");

    const fileInput = host!.querySelector<HTMLInputElement>(".bill-import input[type=file]");
    if (!fileInput) throw new Error("bill import file input not found");
    const file = { name: "draft.csv", size: csv.length, text: async () => csv } as File;
    Object.defineProperty(fileInput, "files", { configurable: true, value: [file] });
    await act(async () => {
      fileInput.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    const renamedCycle = { ...cycleValue, name: "Renamed billing account" };
    const renamedCatalog: Catalog = {
      ...catalog,
      accounts: [{ id: 1, name: "Renamed billing account", platform: "claude", status: "active", deleted: false, created_at: "2026-01-01T00:00:00Z" }],
    };
    await render(<SettleForm {...initialProps} cycles={[renamedCycle]} catalog={renamedCatalog} />);
    await advance(0);

    expect(host!.textContent).toContain("Renamed billing account");
    expect(inputForLabel("新周期成本（元）").value).toBe("66.60");
    const importButton = Array.from(host!.querySelectorAll<HTMLButtonElement>(".bill-import button"))
      .find(value => value.textContent?.trim() === "账单预览");
    expect(importButton?.disabled).toBe(false);
    if (!importButton) throw new Error("bill preview button not found");
    await click(importButton as HTMLButtonElement);
    const importCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/bill-previews"));
    expect(JSON.parse(String(importCall?.[1]?.body))).toMatchObject({ csv, file_name: "draft.csv" });
    expect(previewCalls).toBe(1);
  });

  it("renders multiplier metadata with its status and preserves backend precision", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    let previewCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/previews")) {
        previewCalls += 1;
        const value = cycle({ version: previewCalls });
        const metadata = previewCalls === 1
          ? { cost_multiplier: "0.1367", settled: false }
          : previewCalls === 2
            ? { cost_multiplier: "<0.0001", settled: true }
            : { cost_multiplier: null, settled: false };
        const result = preview(value, `multiplier-preview-${previewCalls}`);
        return response({
          ...result,
          cycles: [{ ...calculation(value), display: metadata }],
        });
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);
    const props = formProps();

    await render(<SettleForm {...props} />);
    await advance(0);
    expect(host!.textContent).toContain("预估倍率");
    expect(host!.textContent).toContain("0.1367×");

    await render(<SettleForm {...props} cycles={[cycle({ version: 2 })]} />);
    await advance(0);
    expect(host!.textContent).toContain("结算倍率");
    expect(host!.textContent).toContain("<0.0001×");

    await render(<SettleForm {...props} cycles={[cycle({ version: 3 })]} />);
    await advance(0);
    const multiplier = Array.from(host!.querySelectorAll(".detail-facts span"))
      .find(value => value.textContent?.includes("倍率"));
    expect(multiplier?.textContent).toContain("预估倍率");
    expect(multiplier?.textContent).toContain("—");
    expect(multiplier?.textContent).not.toContain("×");
    expect(previewCalls).toBe(3);
  });

  it("shows the bill import entry on the first preview and refreshes after confirming an imported bill", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    let previewCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/bill-previews")) {
        return response({
          id: "bill-preview-1",
          cost_fen: 6_000,
          total: "60.00",
          matched_amount: "60.00",
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
          version: 1,
          allocations: [],
          difference_required: false,
          can_confirm: true,
        });
      }
      if (path.endsWith("/previews")) {
        previewCalls += 1;
        return response(preview(cycle({ cost_source: "upstream_bill", bill_import_id: 33 }), `preview-${previewCalls}`));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm {...formProps({ cycles: [cycle({ cost_source: "upstream_bill", bill_import_id: 33 })] })} />);
    await advance(0);

    expect(host!.textContent).toContain("上游账单");
    await click(button("沿用已导入账单"));
    expect(fetcher.mock.calls.some(([input]) => String(input).endsWith("/bill-previews"))).toBe(true);
    expect(host!.textContent).toContain("确认替换成本");

    await click(button("确认替换成本"));
    await advance(0);
    expect(fetcher.mock.calls.some(([input]) => String(input).endsWith("/bill-confirm"))).toBe(true);
    expect(previewCalls).toBe(2);
  });

  it("posts the selected bill allocation mode, invalidates the old result, and hides proportional confirmation for exact matching", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    let billCalls = 0;
    let settlementPreviewCalls = 0;
    const matchedOnlyResponse = deferred<Response>();
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/bill-previews")) {
        billCalls += 1;
        if (billCalls === 1) {
          return response({
            allocation_mode: "proportional",
            id: "bill-preview-proportional",
            cost_fen: 6_000,
            billable_cost_fen: 6_000,
            total: "60.00",
            matched_amount: "50.00",
            unmatched_amount: "10.00",
            matched_count: 2,
            unmatched_count: 1,
            outside_count: 0,
            outside_amount: "0",
            local_unmatched_count: 0,
            record_count: 3,
            file_name: "usage.csv",
            file_hash: "fixture-hash",
            end: "2026-10-01T00:00:00Z",
            version: 1,
            allocations: [],
            difference_required: true,
            can_confirm: true,
          });
        }
        return matchedOnlyResponse.promise;
      }
      if (path.endsWith("/previews")) {
        settlementPreviewCalls += 1;
        return response(preview(cycle({ cost_source: "upstream_bill", bill_import_id: 33, cost_kind: "estimated" }), "settlement-preview-1"));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm {...formProps({ cycles: [cycle({ cost_source: "upstream_bill", bill_import_id: 33, cost_kind: "estimated" })] })} />);
    await advance(0);
    expect(selectForLabel("分摊方式").value).toBe("proportional");

    await click(button("沿用已导入账单"));
    const proportionalCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/bill-previews"));
    expect(JSON.parse(String(proportionalCall?.[1]?.body))).toMatchObject({ allocation_mode: "proportional" });
    expect(host!.textContent).toContain("确认本地差异，按已匹配扣费比例分摊");
    expect(host!.textContent).toContain("确认替换成本");

    const mode = selectForLabel("分摊方式");
    await act(async () => {
      setSelectValue(mode, "matched_only");
      await Promise.resolve();
    });
    await advance(0);

    expect(billCalls).toBe(2);
    expect(host!.textContent).not.toContain("确认本地差异，按已匹配扣费比例分摊");
    expect(host!.textContent).not.toContain("确认替换成本");
    expect(host!.textContent).toContain("匹配中…");
    const matchedOnlyCall = fetcher.mock.calls.filter(([input]) => String(input).endsWith("/bill-previews"))[1];
    expect(JSON.parse(String(matchedOnlyCall?.[1]?.body))).toMatchObject({ allocation_mode: "matched_only" });

    await act(async () => {
      matchedOnlyResponse.resolve(response({
        allocation_mode: "matched_only",
        id: "bill-preview-matched",
        cost_fen: 6_000,
        billable_cost_fen: 5_000,
        total: "60.00",
        matched_amount: "50.00",
        unmatched_amount: "10.00",
        matched_count: 2,
        unmatched_count: 1,
        outside_count: 0,
        outside_amount: "0",
        local_unmatched_count: 0,
        record_count: 3,
        file_name: "usage.csv",
        file_hash: "fixture-hash",
        end: "2026-10-01T00:00:00Z",
        version: 1,
        allocations: [],
        difference_required: false,
        can_confirm: true,
      }));
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(host!.textContent).toContain("上游总扣费");
    expect(host!.textContent).toContain("本次计费");
    expect(host!.textContent).toContain("未匹配不计费");
    expect(host!.textContent).toContain("确认仅按匹配金额计费");
    expect(host!.textContent).not.toContain("确认本地差异，按已匹配扣费比例分摊");

    await click(button("确认仅按匹配金额计费"));
    await advance(0);
    const billConfirmCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/bill-confirm"));
    expect(JSON.parse(String(billConfirmCall?.[1]?.body))).toMatchObject({
      preview_id: "bill-preview-matched",
      accept_difference: false,
    });
    expect(settlementPreviewCalls).toBe(2);
  });

  it("keeps renewal and deletion mutually exclusive and includes optional delete cycle ids in settlement payload", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) return response(preview(cycle(), "settlement-preview-delete"));
      if (path.endsWith("/deletion-scope")) return response({ account_id: 1, digest: "a".repeat(64), targets: [{ id: 1, name: "Shared account", shadow: false }] });
      if (path.endsWith("/settlements")) return response({ deletions: [{ id: 12, account_id: 1, account_name: "Shared account", settlement_id: 88, status: "failed", version: 1, error: "upstream unavailable", updated_at: "2026-10-01T00:00:00Z" }] });
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);
    const done = vi.fn();

    await render(<SettleForm {...formProps({ done })} />);
    await advance(0);
    const renewal = checkbox("结算后续期");
    const deletion = checkbox("出单后删除账号");

    await act(async () => {
      deletion.click();
      await Promise.resolve();
    });
    expect(deletion.checked).toBe(true);
    expect(renewal.checked).toBe(false);
    expect(renewal.disabled).toBe(true);

    await advance(0);
    const scopeConfirmation = checkbox("确认删除以上");
    expect(scopeConfirmation.checked).toBe(false);
    await click(button("确认结算"));
    expect(fetcher.mock.calls.some(([input]) => String(input).endsWith("/settlements"))).toBe(false);

    await act(async () => {
      scopeConfirmation.click();
      await Promise.resolve();
    });
    await click(button("确认结算"));
    const settlementCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/settlements"));
    expect(JSON.parse(String(settlementCall?.[1]?.body))).toMatchObject({
      preview_id: "settlement-preview-delete",
      delete_cycle_ids: [7],
      delete_scopes: { "7": "a".repeat(64) },
    });
    expect(done).toHaveBeenCalledWith("已出单，删除未完成。请在管理视图中查看。");
  });

  it("refreshes the settlement preview after confirming an estimated cycle start", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-01T00:00:00.000Z"));
    let previewCalls = 0;
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = String(input);
      if (path.endsWith("/previews")) {
        previewCalls += 1;
        const value = cycle({ start_mode: "minute_estimate", version: previewCalls, start_confirmed: previewCalls > 1 });
        return response({
          id: `preview-${previewCalls}`,
          expires_at: new Date(Date.now() + 120_000).toISOString(),
          cycles: [calculation(value, { start_mode: "minute_estimate", start_confirmed: previewCalls > 1 })],
        });
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<SettleForm {...formProps({ cycles: [cycle({ start_mode: "minute_estimate" })] })} />);
    await advance(0);
    expect(previewCalls).toBe(1);
    expect(host!.textContent).toContain("确认起点");

    await click(button("确认起点"));
    await advance(0);

    expect(fetcher.mock.calls.some(([input]) => String(input).endsWith("/start-confirm"))).toBe(true);
    expect(previewCalls).toBe(2);
    expect(host!.textContent).not.toContain("确认起点");
  });
});
