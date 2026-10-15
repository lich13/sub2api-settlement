import { describe, expect, it, vi, afterEach } from "vitest";
import {
  api,
  money,
  consumption,
  billedMoney,
  canImportBill,
  canUpgradePaid,
  isOperationalAccount,
  setCsrf,
  amountPattern,
  fromInput,
  inputDate,
  date,
} from "./api";

afterEach(() => vi.unstubAllGlobals());

describe("amount presentation", () => {
  it("retains exact cents including zero and large amounts", () => {
    expect(money(0)).toBe("¥0.00");
    expect(money(12050)).toBe("¥120.50");
    expect(money(100000000001)).toBe("¥1,000,000,000.01");
    expect(consumption("0.0000000001")).toBe("$0.00");
    expect(consumption("2.675")).toBe("$2.68");
  });
  it("distinguishes a nonzero sub-cent bill difference from zero", () => {
    expect(billedMoney("0")).toBe("¥0.00");
    expect(billedMoney("0.00264773")).toBe("不足 ¥0.01");
    expect(billedMoney("1.58407997")).toBe("¥1.58");
  });
  it("accepts at most two decimals without rounding input", () => {
    for (const s of ["0", "100", "120.50", "0.01"])
      expect(amountPattern.test(s)).toBe(true);
    for (const s of ["-1", "1.001", "NaN", "1e2", ""])
      expect(amountPattern.test(s)).toBe(false);
  });
  it("interprets an entered date in Beijing regardless of local timezone", () => {
    expect(fromInput("2026-09-30T00:00:00")).toBe("2026-09-29T16:00:00.000Z");
  });
  it("roundtrips a minute input across the Beijing day boundary without seconds", () => {
    const input = inputDate("2026-09-29T16:00:59.999Z").slice(0, 16);
    expect(input).toBe("2026-09-30T00:00");
    expect(fromInput(input)).toBe("2026-09-29T16:00:00.000Z");
    expect(date(fromInput(input), "minute")).toBe("2026/09/30 00:00");
  });
});

describe("upstream bill entry", () => {
  it("shows the entry for estimated and previously imported cycles only", () => {
    expect(canImportBill({ cost_kind: "estimated", cost_source: "manual" })).toBe(true);
    expect(canImportBill({ cost_kind: "exact", cost_source: "upstream_bill" })).toBe(true);
    expect(canImportBill({ cost_kind: "exact", cost_source: "manual" })).toBe(false);
  });
});

describe("paid upgrade entry", () => {
  it("is available only for free accounts", () => {
    expect(canUpgradePaid({ kind: "free" })).toBe(true);
    expect(canUpgradePaid({ kind: "paid" })).toBe(false);
    expect(canUpgradePaid({ kind: "unconfigured" })).toBe(false);
  });
});

describe("account visibility", () => {
  it("keeps deleted historical accounts out of operational lists", () => {
    expect(isOperationalAccount({ deleted: false })).toBe(true);
    expect(isOperationalAccount({ deleted: true })).toBe(false);
  });
});

it("reuses a mutation key after a lost response, binds the expected user, and starts a new key after success", async () => {
  const fetcher = vi
    .fn()
    .mockRejectedValueOnce(new TypeError("connection lost"))
    .mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ ok: true }),
    });
  vi.stubGlobal("fetch", fetcher);
  setCsrf("csrf-fixture", 3);
  await expect(api("/cycles/42", "PATCH", { cost: "120.50" })).rejects.toThrow(
    "connection lost",
  );
  await api("/cycles/42", "PATCH", { cost: "120.50" });
  await api("/cycles/42", "PATCH", { cost: "120.50" });
  const headers = fetcher.mock.calls.map((c) => c[1].headers);
  expect(headers[0]["Idempotency-Key"]).toBe(headers[1]["Idempotency-Key"]);
  expect(headers[2]["Idempotency-Key"]).not.toBe(headers[1]["Idempotency-Key"]);
  expect(headers[1]["X-Settlement-User"]).toBe("3");
  expect(headers[1]["X-CSRF-Token"]).toBe("csrf-fixture");
});
