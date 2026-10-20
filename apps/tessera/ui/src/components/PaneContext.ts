import { createContext, useContext } from "solid-js";

export interface PaneCtx {
  paneId: string;
  /** Begin dragging this pane (header drag → swap with the drop target). */
  startDrag: (e: PointerEvent) => void;
  /** Whether this pane should show the drawer (menu) button. */
  showMenuButton: () => boolean;
  /** Mobile/dual carousel: index and count, for the page dots. */
  index: () => number;
  count: () => number;
  /** The tile is on screen (active mosaic, current page, not hidden behind another). */
  visible: () => boolean;
}

export const PaneContext = createContext<PaneCtx>();

export function usePane(): PaneCtx {
  const ctx = useContext(PaneContext);
  if (!ctx) throw new Error("usePane outside PaneContext");
  return ctx;
}
