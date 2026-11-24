import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, vi } from "vitest";

export function jsonResponse(
  data: unknown,
  status = 200,
  blobContent = "",
): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => data,
    blob: async () => new Blob([blobContent], { type: "text/csv" }),
  } as Response;
}

export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

export async function click(element: HTMLElement) {
  await act(async () => {
    element.click();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

export async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

export async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

export function setInputValue(
  input: HTMLInputElement | HTMLTextAreaElement,
  value: string,
) {
  act(() => {
    const prototype =
      input instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
    if (!setter) throw new Error("input value setter is unavailable");
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

export function setSelectValue(select: HTMLSelectElement, value: string) {
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(
      HTMLSelectElement.prototype,
      "value",
    )?.set;
    if (!setter) throw new Error("select value setter is unavailable");
    setter.call(select, value);
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

export function renderWithDialog() {
  let root: Root | undefined;
  let host: HTMLDivElement | undefined;
  const nativeShowModal = HTMLDialogElement.prototype.showModal;
  const nativeClose = HTMLDialogElement.prototype.close;

  beforeEach(() => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    HTMLDialogElement.prototype.showModal = function () {
      this.open = true;
    };
    HTMLDialogElement.prototype.close = function () {
      this.open = false;
    };
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
    vi.restoreAllMocks();
  });

  return {
    get host() {
      return host;
    },
    async render(node: ReactNode) {
      if (!root) {
        host = document.createElement("div");
        document.body.append(host);
        root = createRoot(host);
      }
      await act(async () => {
        root!.render(node);
      });
    },
  };
}
