import type { Page } from "@playwright/test";

/** Observe native notice facts before a slow driver can miss the short lifetime.
 * The handle retains measurements only; it never controls application timers,
 * dismissal, rendering, or focus. Call stop and dispose before page teardown. */
export async function observeNativeNotice(page: Page) {
  return page.evaluateHandle(() => {
    const stack = document.querySelector("#toast-stack");
    const history = document.querySelector("#dispatch-history");
    if (!stack || !history) throw new Error("Native notice observer requires both notice roots");
    if (stack.querySelector(".toast.success")) throw new Error("Native notice observer requires an empty success stack");

    const snapshot = (node: Element) => {
      const style = getComputedStyle(node);
      const box = node.getBoundingClientRect();
      return {
        text: node.textContent?.trim() ?? "",
        visible: node.isConnected && box.width > 0 && box.height > 0 && style.visibility === "visible",
        animation: style.animationName,
      };
    };
    const state = {
      appearance: null as ReturnType<typeof snapshot> | null,
      leaving: null as ReturnType<typeof snapshot> | null,
      animations: [] as string[],
      removed: false,
      maxHistoryRows: 0,
    };
    let notice: Element | null = null;
    const sample = () => {
      state.maxHistoryRows = Math.max(state.maxHistoryRows, history.querySelectorAll(".dispatch-history-row").length);
      notice ??= stack.querySelector(".toast.success");
      if (!notice) return;
      if (!notice.isConnected) {
        state.removed = true;
        return;
      }
      const value = snapshot(notice);
      if (!state.appearance && value.visible) state.appearance = value;
      if (!state.leaving && notice.classList.contains("leaving")) state.leaving = value;
    };
    const animationStarted = (event: AnimationEvent) => {
      sample();
      if (event.target === notice) state.animations.push(event.animationName);
    };
    const observer = new MutationObserver(sample);
    observer.observe(stack, { childList: true, subtree: true, attributes: true, characterData: true });
    observer.observe(history, { childList: true, subtree: true });
    stack.addEventListener("animationstart", animationStarted as EventListener);
    sample();
    return {
      state,
      stop() {
        sample();
        observer.disconnect();
        stack.removeEventListener("animationstart", animationStarted as EventListener);
        return state;
      },
    };
  });
}
