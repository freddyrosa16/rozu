import test from 'node:test';
import assert from 'node:assert/strict';
import { sidebarDragFrame, sidebarDragRelease } from '../src/sidebar-motion.js';

test('normal drag tracks pointer one to one and holds readable minimum', () => {
  const drag = { width: 300, x: 300, startedOpen: true, open: true };
  assert.equal(sidebarDragFrame(drag, 360, 520).width, 360);
  assert.equal(sidebarDragFrame(drag, 225, 520).width, 240);
  assert.equal(sidebarDragFrame(drag, 900, 520).width, 520);
});
test('collapse and reversal have different thresholds', () => {
  const drag = { width: 240, x: 240, startedOpen: true, open: true };
  assert.deepEqual(sidebarDragFrame(drag, 180, 520), { open: false, width: 0, animate: true });
  drag.open = false;
  assert.equal(sidebarDragFrame(drag, 200, 520).open, false);
  assert.deepEqual(sidebarDragFrame(drag, 220, 520), { open: true, width: 240, animate: true });
});
test('edge reveal follows pointer without a 200px jump; release settles', () => {
  const drag = { width: 0, x: 4, startedOpen: false, open: false };
  const partial = sidebarDragFrame(drag, 64, 520);
  assert.equal(partial.width, 60);
  assert.deepEqual(sidebarDragRelease(drag, partial), { open: false, width: 0 });
  const committed = sidebarDragFrame(drag, 124, 520);
  assert.equal(committed.width, 120);
  assert.deepEqual(sidebarDragRelease(drag, committed), { open: true, width: 240 });
  assert.equal(sidebarDragRelease(drag, sidebarDragFrame(drag, 324, 520)).width, 320);
});
