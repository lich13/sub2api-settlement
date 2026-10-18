import type { Account, Cycle } from "./types";

let csrf = "";
let currentUser: number | undefined;
const pending = new Map<string, string>();
export class ApiError extends Error {
  constructor(message: string, public status: number) { super(message); }
}
export function setCsrf(value: string, userId?: number) {
  csrf = value;
  currentUser = userId;
}
export async function api<T = any>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  const signature = method + path + JSON.stringify(body);
  const key = pending.get(signature) || crypto.randomUUID();
  if (method !== "GET") pending.set(signature, key);
  const response = await fetch("/settlement/api/v1" + path, {
    method,
    credentials: "same-origin",
    cache: "no-store",
    headers: {
      "Content-Type": "application/json",
      ...(currentUser ? { "X-Settlement-User": String(currentUser) } : {}),
      ...(method === "GET"
        ? {}
        : { "X-CSRF-Token": csrf, "Idempotency-Key": key }),
    },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  });
  const data = await response.json();
  if (response.status < 500) pending.delete(signature);
  if (!response.ok) {
    if (response.status === 401)
      window.dispatchEvent(new Event("settlement:expired"));
    const detail =
      typeof data.detail === "string"
        ? data.detail
        : "输入格式不正确，请检查金额和时间";
    throw new ApiError(detail, response.status);
  }
  return data;
}
export function money(fen: number) {
  const n = Math.abs(fen);
  return (
    (fen < 0 ? "-" : "") +
    "¥" +
    Math.floor(n / 100).toLocaleString("zh-CN") +
    "." +
    String(n % 100).padStart(2, "0")
  );
}
export async function downloadCsv(path: string, filename: string) {
  const response = await fetch("/settlement/api/v1" + path, {
    credentials: "same-origin", cache: "no-store",
    headers: currentUser ? { "X-Settlement-User": String(currentUser) } : {},
  });
  if (!response.ok) {
    if (response.status === 401) window.dispatchEvent(new Event("settlement:expired"));
    const data = await response.json();
    throw new ApiError(typeof data.detail === "string" ? data.detail : "导出失败，请重试", response.status);
  }
  const url = URL.createObjectURL(await response.blob());
  const link = document.createElement("a");
  link.href = url; link.download = filename; document.body.append(link); link.click(); link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export function costInput(fen: number) {
  return (fen / 100).toFixed(2);
}
export function consumption(value: string) {
  return (
    "$" +
    new Intl.NumberFormat("zh-CN", {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(Number(value))
  );
}
export function billedMoney(value: string) {
  const number = Number(value);
  if (number > 0 && number < 0.005) return "不足 ¥0.01";
  return "¥" + new Intl.NumberFormat("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(number);
}

export function canImportBill(cycle: Pick<Cycle, "cost_kind" | "cost_source"> & { vendor_period_id?: number | null }) {
  return !cycle.vendor_period_id;
}

export function canUpgradePaid(account: Pick<Account, "kind">) {
  return account.kind === "free";
}

export function isOperationalAccount(account: Pick<Account, "deleted">) {
  return !account.deleted;
}

export function date(value?: string | null, precision: "second" | "minute" = "second") {
  if (!value) return "进行中";
  return new Intl.DateTimeFormat("zh-CN", {
    timeZone: "Asia/Shanghai",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    ...(precision === "second" ? { second: "2-digit" as const } : {}),
    hour12: false,
  }).format(new Date(value));
}
export function inputDate(value = new Date().toISOString()) {
  return new Date(new Date(value).getTime() + 8 * 3600_000)
    .toISOString()
    .slice(0, 19);
}
export function fromInput(value: string) {
  return new Date(value + "+08:00").toISOString();
}
export const amountPattern = /^(0|[1-9][0-9]{0,9})(\.[0-9]{1,2})?$/;
