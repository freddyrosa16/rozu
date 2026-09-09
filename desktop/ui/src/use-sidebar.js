import { useEffect, useRef, useState } from 'react';
import { SIDEBAR_MIN, SIDEBAR_MAX, sidebarDragFrame, sidebarDragRelease } from './sidebar-motion';

const defaultWidth = () => Math.min(306, Math.max(SIDEBAR_MIN, window.innerWidth * .2));
const maxWidth = () => Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, window.innerWidth - 440));

export function useSidebar() {
  const rootRef = useRef(null);
  const handleRef = useRef(null);
  const [sidebar, setSidebar] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(defaultWidth);
  const [sidebarLimit, setSidebarLimit] = useState(maxWidth);
  const [resizingSidebar, setResizingSidebar] = useState(false);
  const savedWidth = useRef(sidebarWidth);
  const dragRef = useRef(null);
  const raf = useRef(null);

  const paint = (width, animate = false) => {
    const root = rootRef.current;
    if (!root) return;
    root.dataset.sidebarMotion = animate ? 'settle' : 'direct';
    root.style.setProperty('--sidebar-width', `${width}px`);
    if (width > 0) root.style.setProperty('--sidebar-content-width', `${Math.max(SIDEBAR_MIN, width)}px`);
    handleRef.current?.setAttribute('aria-valuenow', String(Math.round(width)));
  };
  const settle = (open, width = savedWidth.current) => {
    const bounded = Math.max(SIDEBAR_MIN, Math.min(maxWidth(), width));
    if (open) savedWidth.current = bounded;
    paint(open ? bounded : 0, true);
    setSidebarWidth(bounded);
    setSidebar(open);
  };
  const flush = () => {
    raf.current = null;
    const drag = dragRef.current;
    if (!drag) return;
    const frame = sidebarDragFrame(drag, drag.latestX, maxWidth());
    // Avoid restarting an in-progress snap on subsequent pointer events.
    if (frame.width !== drag.frame.width) paint(frame.width, frame.animate);
    if (frame.open !== drag.open) setSidebar(frame.open);
    drag.open = frame.open;
    drag.frame = frame;
  };
  const finish = (event, cancelled = false) => {
    const drag = dragRef.current;
    if (!drag || (event?.pointerId != null && event.pointerId !== drag.pointer)) return;
    if (raf.current !== null) cancelAnimationFrame(raf.current);
    raf.current = null;
    if (!cancelled && event?.clientX != null) drag.latestX = event.clientX;
    flush();
    const result = cancelled ? { open: drag.startedOpen, width: drag.width } : sidebarDragRelease(drag, drag.frame);
    dragRef.current = null;
    setResizingSidebar(false);
    settle(result.open, result.open ? result.width : savedWidth.current);
    if (handleRef.current?.hasPointerCapture(drag.pointer)) handleRef.current.releasePointerCapture(drag.pointer);
  };
  useEffect(() => {
    paint(savedWidth.current);
    const fit = () => {
      const maximum = maxWidth();
      setSidebarLimit(maximum);
      savedWidth.current = Math.min(savedWidth.current, maximum);
      setSidebarWidth(savedWidth.current);
      const visible = !rootRef.current?.classList.contains('sidebar-hidden');
      if (!dragRef.current) paint(visible ? savedWidth.current : 0);
    };
    const cancel = () => finish(null, true);
    window.addEventListener('resize', fit);
    window.addEventListener('blur', cancel);
    return () => {
      window.removeEventListener('resize', fit);
      window.removeEventListener('blur', cancel);
      if (raf.current !== null) cancelAnimationFrame(raf.current);
    };
  }, []);

  return {
    rootRef, handleRef, sidebar, sidebarWidth, sidebarLimit, resizingSidebar,
    toggleSidebar: open => settle(open),
    dividerEvents: {
      onPointerDown(event) {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.focus({ preventScroll: true });
        event.currentTarget.setPointerCapture(event.pointerId);
        const width = rootRef.current.querySelector('.sidebar-shell').getBoundingClientRect().width;
        paint(width);
        dragRef.current = { pointer: event.pointerId, x: event.clientX, latestX: event.clientX, width, startedOpen: sidebar && width >= SIDEBAR_MIN, open: sidebar, frame: { open: sidebar, width } };
        setResizingSidebar(true);
      },
      onPointerMove(event) {
        const drag = dragRef.current;
        if (drag?.pointer !== event.pointerId) return;
        const samples = event.nativeEvent.getCoalescedEvents?.();
        drag.latestX = samples?.length ? samples[samples.length - 1].clientX : event.clientX;
        if (raf.current === null) raf.current = requestAnimationFrame(flush);
      },
      onPointerUp: event => finish(event),
      onPointerCancel: event => finish(event, true),
      onLostPointerCapture: event => finish(event, true),
      onDoubleClick: () => settle(true, defaultWidth()),
      onKeyDown(event) {
        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End', 'Enter'].includes(event.key)) return;
        event.preventDefault();
        if (event.key === 'Enter') return settle(!sidebar);
        if (event.key === 'ArrowLeft' && (!sidebar || sidebarWidth <= SIDEBAR_MIN)) return settle(false);
        if (event.key === 'ArrowRight' && !sidebar) return settle(true);
        settle(true, ({ ArrowLeft: sidebarWidth - 10, ArrowRight: sidebarWidth + 10, Home: SIDEBAR_MIN, End: sidebarLimit })[event.key]);
      },
    },
  };
}
