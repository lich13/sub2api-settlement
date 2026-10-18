// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { VendorBillPanel, VendorGroups, VendorManager, expandVendorSelection, groupCandidates } from "./Vendors";
import type { Account, Catalog, Cycle, Vendor, VendorBillPreview, VendorPeriod } from "./types";

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

async function click(button: HTMLButtonElement) {
  await act(async () => {
    button.click();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

function button(text: string) {
  const found = Array.from(host!.querySelectorAll<HTMLButtonElement>("button"))
    .find(element => element.textContent?.trim() === text);
  if (!found) throw new Error(`button not found: ${text}`);
  return found;
}

function inputForLabel(labelText: string) {
  const label = Array.from(host!.querySelectorAll("label"))
    .find(element => element.textContent?.includes(labelText));
  const input = label?.querySelector<HTMLInputElement>("input");
  if (!input) throw new Error(`input not found for label: ${labelText}`);
  return input;
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

function selectForLabel(labelText: string) {
  const label = Array.from(host!.querySelectorAll("label"))
    .find(element => element.textContent?.includes(labelText));
  const select = label?.querySelector<HTMLSelectElement>("select");
  if (!select) throw new Error(`select not found for label: ${labelText}`);
  return select;
}

function candidateCheckbox(name: string) {
  const checkbox = Array.from(host!.querySelectorAll<HTMLInputElement>("fieldset input[type=checkbox]"))
    .find(input => input.closest("label")?.textContent?.includes(name));
  if (!checkbox) throw new Error(`candidate checkbox not found: ${name}`);
  return checkbox;
}

function response(data: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
  } as Response;
}

function cycle(overrides: Partial<Cycle> = {}): Cycle {
  return {
    id: 11,
    account_id: 101,
    name: "Visible account",
    cost_fen: 1_000,
    creditor_id: 7,
    start: "2026-09-01T00:00:00Z",
    end: null,
    status: "open",
    version: 3,
    settlement_id: null,
    cost_kind: "exact",
    ...overrides,
  };
}

function period(overrides: Partial<VendorPeriod> = {}): VendorPeriod {
  return {
    id: 21,
    vendor_id: 5,
    name: "Shared provider",
    cost_fen: 12_345,
    creditor_id: 7,
    creditor_name: "Alice",
    start: "2026-09-01T00:00:00Z",
    end: null,
    start_confirmed: true,
    version: 4,
    status: "open",
    members: [],
    ...overrides,
  };
}

function vendor(value: VendorPeriod): Vendor {
  return {
    id: 5,
    name: "Shared provider",
    cost_fen: 12_345,
    creditor_id: 7,
    creditor_name: "Alice",
    version: 2,
    archived_at: null,
    periods: [value],
  };
}

function account(id: number, name: string, value: Cycle, overrides: Partial<Account> = {}): Account {
  return {
    id,
    name,
    platform: "claude",
    kind: "paid",
    deleted: false,
    profile_version: 1,
    cycle: value,
    ...overrides,
  };
}

function unconfiguredAccount(id: number, name: string, overrides: Partial<Account> = {}): Account {
  return {
    id,
    name,
    platform: "claude",
    kind: "unconfigured",
    deleted: false,
    profile_version: null,
    cycle: null,
    ...overrides,
  };
}

function catalog(): Catalog {
  return {
    accounts: [],
    users: [{ id: 7, name: "Alice" }],
    coverage_from: "2026-01-01T00:00:00Z",
  };
}

function billPreview(overrides: Partial<VendorBillPreview> = {}): VendorBillPreview {
  return {
    id: "vendor-bill-preview",
    cost_fen: 10_000,
    bill_total_fen: 10_000,
    total: "100.00",
    matched_amount: "100.00",
    unmatched_amount: "0",
    matched_count: 1,
    unmatched_count: 0,
    outside_count: 0,
    outside_amount: "0",
    local_unmatched_count: 0,
    record_count: 1,
    file_name: "usage.csv",
    file_hash: "fixture-hash",
    end: "2026-10-01T00:00:00Z",
    version: 1,
    allocations: [],
    difference_required: false,
    can_confirm: true,
    accounts: [{
      account_id: 101,
      cycle_id: 11,
      name: "Visible account",
      amount_fen: 10_000,
      total_consumption: "1.00",
      allocations: [],
    }],
    basis: "matched_upstream",
    coverage_warnings: [],
    ...overrides,
  };
}

function setFile(input: HTMLInputElement, name: string, text: string) {
  const file = { name, size: text.length, text: async () => text } as File;
  Object.defineProperty(input, "files", { configurable: true, value: [file] });
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
  vi.unstubAllGlobals();
});

describe("vendor group selection and presentation", () => {
  it("expands a selected member to every cycle in its period, including deleted financial members", () => {
    const deleted = cycle({ id: 12, account_id: 102, name: "Deleted account", account_deleted: true });
    const other = cycle({ id: 13, account_id: 103, name: "Another account" });
    expect(expandVendorSelection([11], [period({ members: [cycle(), deleted, other] })]))
      .toEqual([11, 12, 13]);
  });

  it("hides deleted members, shows pending bill splits instead of zero, and keeps group cost singular", async () => {
    const deleted = cycle({ id: 12, account_id: 102, name: "Deleted account", account_deleted: true });
    const visible = cycle({ id: 11, account_id: 101, name: "Visible account" });
    const value = period({ members: [visible, deleted] });
    const onSettle = vi.fn();

    await render(<VendorGroups
      vendors={[vendor(value)]}
      accounts={[account(101, "Visible account", visible, { allocation_pending: true })]}
      admin
      query=""
      filter="all"
      onSettle={onSettle}
      onManage={vi.fn()}
      onDetail={vi.fn()}
    />);

    expect(host!.textContent).toContain("1 个账号");
    expect(host!.textContent).toContain("待账单拆分");
    expect(host!.textContent).not.toContain("Deleted account");
    expect(host!.textContent).not.toContain("¥0.00");
    expect(host!.textContent!.match(/¥123\.45/g)).toHaveLength(1);

    await click(button("结算并续费"));
    expect(onSettle).toHaveBeenCalledWith([11, 12], true);
  });
});

describe("vendor group manager v0.11.1 flows", () => {
  it("creates one group and its first period in one atomic request", async () => {
    const first = unconfiguredAccount(101, "Unconfigured Alpha");
    const second = unconfiguredAccount(102, "Cancelled draft Beta", {
      profile_version: null,
      cycle: cycle({ id: 12, account_id: 102, name: "Cancelled draft Beta", status: "cancelled", end: "2026-08-31T00:00:00Z" }),
    });
    const fetcher = vi.fn<typeof fetch>(async () => response({ id: 5 }));
    const refresh = vi.fn(async () => {});
    const close = vi.fn();
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorManager
      vendors={[]}
      accounts={[first, second]}
      catalog={catalog()}
      close={close}
      refresh={refresh}
      initialAction="create"
    />);

    setInputValue(inputForLabel("账号组名称"), "Shared provider");
    setInputValue(inputForLabel("本期准确总成本（元）"), "12.34");
    setInputValue(inputForLabel("共同起点（北京时间）"), "2026-09-01T08:00:00");
    setSelectValue(selectForLabel("起点方式"), "exact");
    await act(async () => {
      candidateCheckbox("Unconfigured Alpha").click();
      candidateCheckbox("Cancelled draft Beta").click();
    });
    await click(button("保存"));

    expect(fetcher).toHaveBeenCalledTimes(1);
    const [path, options] = fetcher.mock.calls[0];
    expect(String(path)).toBe("/settlement/api/v1/vendors");
    expect(options?.method).toBe("POST");
    expect(JSON.parse(String(options?.body))).toEqual({
      name: "Shared provider",
      cost: "12.34",
      creditor_id: 7,
      start: "2026-09-01T00:00:00.000Z",
      start_mode: "exact",
      account_ids: [101, 102],
    });
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("offers only unconfigured, live, unassigned accounts, including cancelled no-profile drafts", async () => {
    const eligible = unconfiguredAccount(101, "Eligible");
    const cancelledDraft = unconfiguredAccount(102, "Cancelled no-profile draft", {
      cycle: cycle({ id: 12, account_id: 102, status: "cancelled", end: "2026-08-31T00:00:00Z" }),
    });
    const deleted = unconfiguredAccount(103, "Deleted", { deleted: true });
    const assigned = unconfiguredAccount(104, "Already assigned", { vendor_id: 5 });
    const configured = account(105, "Configured", cycle({ id: 15, account_id: 105 }));

    expect(groupCandidates([eligible, cancelledDraft, deleted, assigned, configured]).map(a => a.id))
      .toEqual([101, 102]);

    await render(<VendorManager
      vendors={[]}
      accounts={[eligible, cancelledDraft, deleted, assigned, configured]}
      catalog={catalog()}
      close={vi.fn()}
      refresh={vi.fn(async () => {})}
      initialAction="create"
    />);
    expect(host!.textContent).toContain("Eligible");
    expect(host!.textContent).toContain("Cancelled no-profile draft");
    expect(host!.textContent).not.toContain("Deleted");
    expect(host!.textContent).not.toContain("Already assigned");
    expect(host!.textContent).not.toContain("Configured");
  });

  it("adds members to an open period with its active period version", async () => {
    const value = period({ members: [cycle()] });
    const fetcher = vi.fn<typeof fetch>(async () => response({}));
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorManager
      vendors={[vendor(value)]}
      accounts={[unconfiguredAccount(102, "New member")]}
      catalog={catalog()}
      close={vi.fn()}
      refresh={vi.fn(async () => {})}
      initialVendorId={5}
      initialAction="add"
    />);
    await act(async () => { candidateCheckbox("New member").click(); });
    await click(button("保存"));

    const [path, options] = fetcher.mock.calls[0];
    expect(String(path)).toBe("/settlement/api/v1/vendors/5/members");
    expect(JSON.parse(String(options?.body))).toEqual({
      version: 2,
      period_version: 4,
      account_ids: [102],
    });
  });

  it("adds members to a settled group as a next-period draft with a null period version", async () => {
    const settled = period({ status: "settled", end: "2026-10-01T00:00:00Z", members: [cycle({ status: "settled", end: "2026-10-01T00:00:00Z" })] });
    const fetcher = vi.fn<typeof fetch>(async () => response({}));
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorManager
      vendors={[vendor(settled)]}
      accounts={[unconfiguredAccount(102, "Next period member")]}
      catalog={catalog()}
      close={vi.fn()}
      refresh={vi.fn(async () => {})}
      initialVendorId={5}
      initialAction="add"
    />);
    expect(host!.textContent).toContain("新增成员从下一期生效，已结算账期保持不变。");
    await act(async () => { candidateCheckbox("Next period member").click(); });
    await click(button("保存"));

    const [path, options] = fetcher.mock.calls[0];
    expect(String(path)).toBe("/settlement/api/v1/vendors/5/members");
    expect(JSON.parse(String(options?.body))).toEqual({
      version: 2,
      period_version: null,
      account_ids: [102],
    });
  });

  it("renews the whole settled group without a member selection payload", async () => {
    const settled = period({ status: "settled", end: "2026-10-01T00:00:00Z", members: [cycle({ status: "settled", end: "2026-10-01T00:00:00Z" })] });
    const fetcher = vi.fn<typeof fetch>(async () => response({}));
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorManager
      vendors={[vendor(settled)]}
      accounts={[]}
      catalog={catalog()}
      close={vi.fn()}
      refresh={vi.fn(async () => {})}
      initialVendorId={5}
      initialAction="renew"
    />);
    expect(host!.querySelectorAll('input[type="checkbox"]')).toHaveLength(0);
    setInputValue(inputForLabel("下期准确总成本（元）"), "15.00");
    await click(button("确认整组续费"));

    const [path, options] = fetcher.mock.calls[0];
    expect(String(path)).toBe("/settlement/api/v1/vendors/5/renew");
    const body = JSON.parse(String(options?.body));
    expect(body).toEqual({
      version: 2,
      period_version: 4,
      cost: "15.00",
      creditor_id: 7,
    });
    expect(body).not.toHaveProperty("account_ids");
    expect(body).not.toHaveProperty("members");
  });

  it("keeps settled groups and current members visible with one group cost", async () => {
    const settledCycle = cycle({ id: 11, status: "settled", end: "2026-10-01T00:00:00Z", settlement_id: 9 });
    const settled = period({ status: "settled", end: "2026-10-01T00:00:00Z", members: [settledCycle] });
    const settledVendor = {
      ...vendor(settled),
      current_members: [{ account_id: 101, name: "Visible account", account_deleted: false, profile_version: 2 }],
    };
    const onManage = vi.fn();

    await render(<VendorGroups
      vendors={[settledVendor]}
      accounts={[account(101, "Visible account", settledCycle, { mine: null })]}
      admin
      query=""
      filter="all"
      onSettle={vi.fn()}
      onManage={onManage}
      onDetail={vi.fn()}
    />);

    expect(host!.textContent).toContain("Shared provider");
    expect(host!.textContent).toContain("Visible account");
    expect(host!.textContent).toContain("已结算／待续费");
    expect(host!.textContent!.match(/¥123\.45/g)).toHaveLength(1);
    expect(host!.textContent).not.toContain("修改组成本");
    expect(host!.textContent).not.toContain("整组结算");

    await click(button("整组续费"));
    expect(onManage).toHaveBeenCalledWith(5, "renew");
  });
});

describe("vendor manager", () => {
  it("keeps deleted period members in the complete group save payload", async () => {
    const deleted = cycle({ id: 12, account_id: 102, name: "Deleted account", account_deleted: true });
    const value = period({ members: [cycle(), deleted] });
    const fetcher = vi.fn<typeof fetch>(async () => response({}));
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorManager
      vendors={[vendor(value)]}
      accounts={[]}
      catalog={catalog()}
      close={vi.fn()}
      refresh={vi.fn(async () => {})}
    />);
    await click(button("整组配置"));

    expect(host!.textContent).toContain("Deleted account · 已删除，保留本期账务");
    await click(button("保存"));

    const saveCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/vendor-periods/21"));
    expect(saveCall).toBeDefined();
    expect(JSON.parse(String(saveCall?.[1]?.body))).toMatchObject({
      version: 4,
      members: [
        { cycle_id: 11, version: 3 },
        { cycle_id: 12, version: 3 },
      ],
      detached: [],
    });
  });
});

describe("vendor bill panel", () => {
  it("requires explicit confirmation for a bill difference", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/bill-previews")) {
        return response(billPreview({
          id: "difference-preview",
          bill_total_fen: 9_900,
          matched_amount: "99.00",
          unmatched_amount: "1.00",
          unmatched_count: 1,
          difference_required: true,
        }));
      }
      return response({});
    });
    const onConfirmed = vi.fn(async () => {});
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorBillPanel
      period={period({ version: 4, start_confirmed: true })}
      cycles={[cycle()]}
      end="2026-10-01T00:00:00Z"
      previewKey="initial"
      onConfirmed={onConfirmed}
    />);

    const fileInput = host!.querySelector<HTMLInputElement>("input[type=file]");
    if (!fileInput) throw new Error("bill file input not found");
    setFile(fileInput, "usage.csv", "timestamp,cost\n2026-09-20T00:00:00Z,99\n");
    await act(async () => {
      fileInput.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
      await Promise.resolve();
    });
    await click(button("账单预览"));

    const confirmButton = button("确认分摊依据");
    expect(confirmButton.disabled).toBe(true);
    const difference = Array.from(host!.querySelectorAll<HTMLInputElement>("input[type=checkbox]"))
      .find(input => input.closest("label")?.textContent?.includes("确认组成本与账单"));
    if (!difference) throw new Error("difference confirmation checkbox not found");
    await act(async () => { difference.click(); await Promise.resolve(); });
    expect(confirmButton.disabled).toBe(false);

    await click(confirmButton);
    const confirmCall = fetcher.mock.calls.find(([input]) => String(input).endsWith("/bill-confirm"));
    expect(JSON.parse(String(confirmCall?.[1]?.body))).toMatchObject({
      version: 4,
      preview_id: "difference-preview",
      accept_difference: true,
      accept_outside: false,
    });
    expect(onConfirmed).toHaveBeenCalledTimes(1);
  });

  it("explains the local consumption fallback and refuses a no-match split without a positive basis", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/bill-previews")) {
        return response(billPreview({
          id: "local-fallback",
          cost_fen: 10_000,
          bill_total_fen: 0,
          total: "0.00",
          matched_amount: "0",
          unmatched_amount: "10.00",
          matched_count: 0,
          unmatched_count: 1,
          basis: "local_consumption",
          can_confirm: false,
          accounts: [{
            account_id: 101,
            cycle_id: 11,
            name: "Visible account",
            amount_fen: null,
            total_consumption: "0",
            allocations: [],
          }],
        }));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);

    await render(<VendorBillPanel
      period={period({ start_confirmed: true })}
      cycles={[cycle()]}
      end="2026-10-01T00:00:00Z"
      previewKey="local"
      onConfirmed={vi.fn(async () => {})}
    />);
    const fileInput = host!.querySelector<HTMLInputElement>("input[type=file]");
    if (!fileInput) throw new Error("bill file input not found");
    setFile(fileInput, "no-match.csv", "timestamp,cost\n2026-09-20T00:00:00Z,10\n");
    await act(async () => {
      fileInput.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
      await Promise.resolve();
    });
    await click(button("账单预览"));

    expect(host!.textContent).toContain("无正金额匹配，按各账号本地原始消费分摊。");
    expect(host!.textContent).toContain("没有正金额匹配或本地消费依据，无法分摊");
    expect(host!.textContent).toContain("Visible account—");
    expect(host!.textContent).not.toContain("Visible account¥0.00");
    expect(button("确认分摊依据").disabled).toBe(true);
  });

  it("keeps the CSV draft while version, cutoff, and preview key invalidate the old preview", async () => {
    const fetcher = vi.fn<typeof fetch>(async input => {
      if (String(input).endsWith("/bill-previews")) {
        const body = JSON.parse(String(fetcher.mock.calls.at(-1)?.[1]?.body));
        return response(billPreview({
          id: `preview-v${body.version}`,
          file_name: "draft.csv",
          accounts: [{
            account_id: 101,
            cycle_id: 11,
            name: `preview-v${body.version}`,
            amount_fen: 10_000,
            total_consumption: "1.00",
            allocations: [],
          }],
        }));
      }
      return response({});
    });
    vi.stubGlobal("fetch", fetcher);
    const initialPeriod = period({ version: 4, start_confirmed: true });
    const updatedPeriod = period({ version: 5, start_confirmed: true });
    const end = "2026-10-01T00:00:00Z";

    await render(<VendorBillPanel
      period={initialPeriod}
      cycles={[cycle()]}
      end={end}
      previewKey="initial"
      onConfirmed={vi.fn(async () => {})}
    />);
    const fileInput = host!.querySelector<HTMLInputElement>("input[type=file]");
    if (!fileInput) throw new Error("bill file input not found");
    setFile(fileInput, "draft.csv", "timestamp,cost\n2026-09-20T00:00:00Z,10\n");
    await act(async () => {
      fileInput.dispatchEvent(new Event("change", { bubbles: true }));
      await Promise.resolve();
      await Promise.resolve();
    });
    await click(button("账单预览"));
    expect(host!.textContent).toContain("preview-v4");

    await render(<VendorBillPanel
      period={updatedPeriod}
      cycles={[cycle()]}
      end="2026-10-02T00:00:00Z"
      previewKey="updated"
      onConfirmed={vi.fn(async () => {})}
    />);
    expect(host!.textContent).not.toContain("preview-v4");
    const updatedFileInput = host!.querySelector<HTMLInputElement>("input[type=file]");
    expect(updatedFileInput?.files?.[0]?.name).toBe("draft.csv");

    await click(button("账单预览"));
    const billCalls = fetcher.mock.calls.filter(([input]) => String(input).endsWith("/bill-previews"));
    expect(billCalls).toHaveLength(2);
    expect(JSON.parse(String(billCalls[1][1]?.body))).toMatchObject({
      version: 5,
      end: "2026-10-02T00:00:00Z",
      csv: "timestamp,cost\n2026-09-20T00:00:00Z,10\n",
      file_name: "draft.csv",
    });
  });
});
