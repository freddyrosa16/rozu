const {test} = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(require('node:path').join(__dirname, '../waves.js'), 'utf8');
function setup(reducedMotion) {
  let next = 0;
  const frames = new Map();
  const handlers = {};
  let draws = 0;
  const canvasHandlers = {};
  const documentHandlers = {};
  const media = {matches: reducedMotion, addEventListener(type, fn) {this.change = fn;}};
  const label = {textContent: ''};
  const button = {hidden: true, classList: {toggle(){}}, setAttribute(key,value){this[key]=value;}, addEventListener(type,fn){handlers[type]=fn;}};
  const canvas = {dataset:{},addEventListener:(type,fn)=>{canvasHandlers[type]=fn;},getBoundingClientRect:()=>({width:60,height:60}),getContext:type=>type==='webgl'?null:({createImageData:(w,h)=>({data:new Uint8ClampedArray(w*h*4)}),putImageData(){draws++;}})};
  const body = {dataset:{theme:'graphite'}};
  const document = {body, hidden:false, getElementById:id=>({'waves':canvas,'motion-toggle':button,'motion-label':label}[id]),querySelector:()=>({content:''}),addEventListener(type,fn){documentHandlers[type]=fn;}};
  vm.runInNewContext(source,{document,matchMedia:()=>media,getComputedStyle:()=>({getPropertyValue:key=>key==='--wave-rgb'?'100,145,196':'11,13,16'}),requestAnimationFrame:fn=>{frames.set(++next,fn);return next;},cancelAnimationFrame:id=>frames.delete(id),ResizeObserver:class{observe(){}},IntersectionObserver:class{observe(){}}});
  return {frames,handlers,media,label,button,document,documentHandlers,canvas,canvasHandlers,drawCount:()=>draws};
}
test('reduced motion starts still; explicit toggle can play and pause',()=>{
  const app=setup(true);
  assert.equal(app.label.textContent,'Play animation');
  assert.equal(app.frames.size,0);
  assert.equal(app.button.hidden,false);
  app.handlers.click();
  assert.equal(app.label.textContent,'Pause animation');
  assert.equal(app.frames.size,1);
  app.handlers.click();
  assert.equal(app.frames.size,0);
});
test('hidden tabs suspend drawing; visible tabs resume without duplicate loops',()=>{
  const app=setup(false);
  assert.equal(app.frames.size,1);
  app.document.hidden=true;app.documentHandlers.visibilitychange();
  assert.equal(app.frames.size,0);
  app.document.hidden=false;app.documentHandlers.visibilitychange();app.documentHandlers.visibilitychange();
  assert.equal(app.frames.size,1);
});
test('updated reduced-motion preference pauses an existing animation',()=>{
  const app=setup(false);
  app.media.matches=true;app.media.change();
  assert.equal(app.frames.size,0);
  assert.equal(app.label.textContent,'Play animation');
});

test('fallback draws at 30fps instead of the old 15fps cap',()=>{
  const app=setup(false);
  function advance(now){const [id,fn]=app.frames.entries().next().value;app.frames.delete(id);fn(now);}
  const before=app.drawCount();
  advance(1000);advance(1034);advance(1068);
  assert.equal(app.drawCount()-before,3);
  assert.equal(app.frames.size,1);
  assert.equal(app.canvas.dataset.renderer,'2d');
});
test('graphics context loss stops the loop and restoration resumes it',()=>{
  const app=setup(false);
  let prevented=false;
  app.canvasHandlers.webglcontextlost({preventDefault(){prevented=true;}});
  assert.equal(prevented,true);
  assert.equal(app.frames.size,0);
  app.canvasHandlers.webglcontextrestored();
  assert.equal(app.frames.size,1);
});
