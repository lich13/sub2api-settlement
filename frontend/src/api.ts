let csrf = "";
let currentUser: number | undefined;
const pending = new Map<string, string>();
export const APP_VERSION =
  typeof __APP_VERSION__ !== "undefined" ? __APP_VERSION__ : "";
export function formatVersion(value = APP_VERSION) {
  const trimmed = value.trim();
  if (!trimmed) return "";
  return trimmed.startsWith("v") ? trimmed : `v${trimmed}`;
}
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
const integerFormat = new Intl.NumberFormat("zh-CN");
const decimal2Format = new Intl.NumberFormat("zh-CN", {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});
const dateSecondFormat = new Intl.DateTimeFormat("zh-CN", {
  timeZone: "Asia/Shanghai",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});
const dateMinuteFormat = new Intl.DateTimeFormat("zh-CN", {
  timeZone: "Asia/Shanghai",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});
const MAX_CSV_BYTES = 10 * 1024 * 1024;

export function money(fen: number) {
  const n = Math.abs(fen);
  return (
    (fen < 0 ? "-" : "") +
    "¥" +
    integerFormat.format(Math.floor(n / 100)) +
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
export type CsvFileResult =
  | { status: "empty" }
  | { status: "too_large"; error: string }
  | { status: "stale" }
  | { status: "ok"; name: string; content: string };

export async function readCsvFile(
  file: File | undefined,
  counter: { current: number },
  onReset?: () => void,
): Promise<CsvFileResult> {
  const token = ++counter.current;
  onReset?.();
  if (!file) return { status: "empty" };
  if (file.size > MAX_CSV_BYTES) {
    return { status: "too_large", error: "CSV 不能超过 10 MB" };
  }
  const content = await file.text();
  if (token !== counter.current) return { status: "stale" };
  return { status: "ok", name: file.name, content };
}

export async function exportUnmatchedCsv(
  path: string,
  filename: string,
  versionRef: { current: number },
  setBusy: (busy: boolean) => void,
  setError: (error: string) => void,
) {
  const token = versionRef.current;
  setBusy(true);
  setError("");
  try {
    await downloadCsv(path, filename);
  } catch (e) {
    if (token === versionRef.current) setError((e as Error).message);
  } finally {
    if (token === versionRef.current) setBusy(false);
  }
}

export function costInput(fen: number) {
  return (fen / 100).toFixed(2);
}
export function consumption(value: string) {
  return "$" + decimal2Format.format(Number(value));
}
export function billedMoney(value: string) {
  const number = Number(value);
  if (number > 0 && number < 0.005) return "不足 ¥0.01";
  return "¥" + decimal2Format.format(number);
}

export function date(value?: string | null, precision: "second" | "minute" = "second") {
  if (!value) return "进行中";
  return (precision === "second" ? dateSecondFormat : dateMinuteFormat).format(
    new Date(value),
  );
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
export const percentPattern = /^(0|[1-9][0-9]{0,2})(\.[0-9]{1,2})?$/;
export const ratePattern = /^(?:0|[1-9][0-9]*)(?:\.[0-9]{1,6})?$/;
