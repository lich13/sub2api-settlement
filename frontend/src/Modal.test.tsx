// @vitest-environment jsdom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Modal } from "./Modal";

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
  vi.unstubAllGlobals();
});

describe("Modal draft and IME guards", () => {
  it("closes on backdrop click and cancel when clean, but blocks both when dirty", async () => {
    const close = vi.fn();
    await render(
      <Modal title="编辑窗口" close={close}>
        <input defaultValue="clean" />
      </Modal>,
    );
    const dialog = host!.querySelector("dialog")!;

    await act(async () => {
      dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(close).toHaveBeenCalledTimes(1);

    await render(
      <Modal title="编辑窗口" dirty close={close}>
        <input defaultValue="dirty" />
      </Modal>,
    );
    await act(async () => {
      dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      dialog.dispatchEvent(new Event("cancel", { cancelable: true }));
      dialog.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Escape",
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    expect(close).toHaveBeenCalledTimes(1);

    const closeBtn = host!.querySelector<HTMLButtonElement>(
      'button[aria-label="关闭"]',
    )!;
    await act(async () => {
      closeBtn.click();
    });
    expect(close).toHaveBeenCalledTimes(2);
  });

  it("blocks Escape dismissal while IME composition is active", async () => {
    const close = vi.fn();
    await render(
      <Modal title="输入窗口" close={close}>
        <input />
      </Modal>,
    );
    const input = host!.querySelector("input")!;
    const dialog = host!.querySelector("dialog")!;

    await act(async () => {
      input.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
      input.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Escape",
          bubbles: true,
          cancelable: true,
        }),
      );
      dialog.dispatchEvent(new Event("cancel", { cancelable: true }));
    });
    expect(close).not.toHaveBeenCalled();

    await act(async () => {
      input.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
      input.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Escape",
          keyCode: 229,
          bubbles: true,
          cancelable: true,
        }),
      );
      dialog.dispatchEvent(new Event("cancel", { cancelable: true }));
    });
    expect(close).not.toHaveBeenCalled();

    await act(async () => {
      input.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Escape",
          keyCode: 27,
          bubbles: true,
          cancelable: true,
        }),
      );
      dialog.dispatchEvent(new Event("cancel", { cancelable: true }));
    });
    expect(close).toHaveBeenCalledTimes(1);
  });
});
