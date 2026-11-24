// @vitest-environment jsdom

import { act } from "react";
import { describe, expect, it, vi } from "vitest";
import { Modal } from "./Modal";
import { renderWithDialog } from "./test-utils";

const dom = renderWithDialog();

describe("Modal draft and IME guards", () => {
  it("closes on backdrop click and cancel when clean, but blocks both when dirty", async () => {
    const close = vi.fn();
    await dom.render(
      <Modal title="编辑窗口" close={close}>
        <input defaultValue="clean" />
      </Modal>,
    );
    const dialog = dom.host!.querySelector("dialog")!;

    await act(async () => {
      dialog.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(close).toHaveBeenCalledTimes(1);

    await dom.render(
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

    const closeBtn = dom.host!.querySelector<HTMLButtonElement>(
      'button[aria-label="关闭"]',
    )!;
    await act(async () => {
      closeBtn.click();
    });
    expect(close).toHaveBeenCalledTimes(2);
  });

  it("blocks Escape dismissal while IME composition is active", async () => {
    const close = vi.fn();
    await dom.render(
      <Modal title="输入窗口" close={close}>
        <input />
      </Modal>,
    );
    const input = dom.host!.querySelector("input")!;
    const dialog = dom.host!.querySelector("dialog")!;

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
