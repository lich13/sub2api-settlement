import { describe, expect, it, vi, afterEach } from "vitest";
import {
  APP_VERSION,
  ApiError,
  api,
  formatVersion,
  money,
  consumption,
  billedMoney,
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
  it("rejects empty or malformed datetime input with a Chinese validation message", () => {
    for (const invalid of ["", "   ", "not-a-date", "2026-13-45T99:99"]) {
      expect(() => fromInput(invalid)).toThrow("请填写有效的时间");
    }
  });
  it("roundtrips a minute input across the Beijing day boundary without seconds", () => {
    const input = inputDate("2026-09-29T16:00:59.999Z").slice(0, 16);
    expect(input).toBe("2026-09-30T00:00");
    expect(fromInput(input)).toBe("2026-09-29T16:00:00.000Z");
    expect(date(fromInput(input), "minute")).toBe("2026/09/30 00:00");
  });
  it("formats authoritative and fallback versions with a single v prefix", () => {
    expect(APP_VERSION).toMatch(/^\d+\.\d+\.\d+$/);
    expect(formatVersion()).toBe(`v${APP_VERSION}`);
    expect(formatVersion("0.13.1")).toBe("v0.13.1");
    expect(formatVersion("v0.13.1")).toBe("v0.13.1");
    expect(formatVersion("  ")).toBe("");
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

it("omits mutation headers on GET, handles non-JSON responses safely, dispatches expiry on 401, and clears pending keys on session reset", async () => {
  const win = new EventTarget();
  let expiredEvents = 0;
  win.addEventListener("settlement:expired", () => {
    expiredEvents += 1;
  });
  vi.stubGlobal("window", win);

  const nonJson = (status: number, ok = false) => ({
    ok,
    status,
    json: async () => {
      throw new SyntaxError("Unexpected token < in JSON");
    },
  });
  const fetcher = vi
    .fn()
    .mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => ({ ok: true }),
    })
    .mockResolvedValueOnce(nonJson(502))
    .mockResolvedValueOnce({
      ok: true,
      status: 200,
      json: async () => ({ ok: true }),
    })
    .mockResolvedValueOnce(nonJson(401))
    .mockResolvedValueOnce(nonJson(200, true));
  vi.stubGlobal("fetch", fetcher);

  setCsrf("csrf-session-a", 7);
  await api("/overview");
  const getHeaders = fetcher.mock.calls[0][1].headers;
  expect(getHeaders["X-Settlement-User"]).toBe("7");
  expect(getHeaders["Idempotency-Key"]).toBeUndefined();
  expect(getHeaders["X-CSRF-Token"]).toBeUndefined();
  expect(getHeaders["Content-Type"]).toBeUndefined();

  await expect(
    api("/cycles/9", "PATCH", { cost: "88.00" }),
  ).rejects.toMatchObject({
    message: "输入格式不正确，请检查金额和时间",
    status: 502,
  });
  const keyBeforeReset = fetcher.mock.calls[1][1].headers["Idempotency-Key"];
  expect(keyBeforeReset).toBeTruthy();

  // Clearing session resets cached >=500 idempotency keys so a new session does not reuse them
  setCsrf("");
  setCsrf("csrf-session-b", 7);
  await api("/cycles/9", "PATCH", { cost: "88.00" });
  expect(fetcher.mock.calls[2][1].headers["Idempotency-Key"]).not.toBe(
    keyBeforeReset,
  );

  await expect(api("/inbox/read", "POST")).rejects.toBeInstanceOf(ApiError);
  expect(expiredEvents).toBe(1);

  await expect(api("/overview")).rejects.toMatchObject({
    message: "服务器返回格式异常，请稍后重试",
    status: 200,
  });
});
