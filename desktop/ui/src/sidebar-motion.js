export const SIDEBAR_MIN = 240;
export const SIDEBAR_MAX = 520;
export const COLLAPSE_AT = 192;
export const REOPEN_AT = 216;
export const REVEAL_COMMIT = 80;

export function sidebarDragFrame(drag, pointerX, maximum) {
  const raw = drag.width + pointerX - drag.x;
  if (!drag.startedOpen) {
    // Pulling out from the edge follows the pointer, even below minimum width.
    // The inner sidebar stays full width and is clipped by its outer shell.
    return { open: raw > 0, width: Math.max(0, Math.min(maximum, raw)), animate: false };
  }
  const open = drag.open ? raw >= COLLAPSE_AT : raw >= REOPEN_AT;
  return { open, width: open ? Math.max(SIDEBAR_MIN, Math.min(maximum, raw)) : 0, animate: open !== drag.open };
}

export function sidebarDragRelease(drag, frame) {
  const open = drag.startedOpen ? frame.open : frame.width >= REVEAL_COMMIT;
  return { open, width: open ? Math.max(SIDEBAR_MIN, frame.width) : 0 };
}
