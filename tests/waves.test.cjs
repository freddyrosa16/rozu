const {test} = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(require('node:path').join(__dirname, '../waves.js'), 'utf8');
function setup(reducedMotion) {
  let next = 0;
  const frames = new Map();
  const handlers = {};
  const paletteHandlers = {};
  const documentHandlers = {};
  const media = {matches: reducedMotion, addEventListener(type, fn) {this.change = fn;}};
  const label = {textContent: ''};
  const button = {hidden: true, classList: {toggle(){}}, setAttribute(key,value){this[key]=value;}, addEventListener(type,fn){handlers[type]=fn;}};
  const canvas = {getBoundingClientRect:()=>({width:60,height:60}),getContext:()=>({createImageData:(w,h)=>({data:new Uint8ClampedArray(w*h*4)}),putImageData(){}})};
  const body = {dataset:{theme:'blue'}};
  const radios = ['blue','graphite'].map(value=>({value,addEventListener(type,fn){paletteHandlers[value]=fn;}}));
  const document = {body, hidden:false, getElementById:id=>({'waves':canvas,'motion-toggle':button,'motion-label':label}[id]),querySelectorAll:()=>radios,querySelector:()=>({content:''}),addEventListener(type,fn){documentHandlers[type]=fn;}};
  vm.runInNewContext(source,{document,matchMedia:()=>media,getComputedStyle:()=>({getPropertyValue:key=>key==='--wave-rgb'?'100,145,196':'11,13,16'}),requestAnimationFrame:fn=>{frames.set(++next,fn);return next;},cancelAnimationFrame:id=>frames.delete(id),ResizeObserver:class{observe(){}},IntersectionObserver:class{observe(){}}});
  return {frames,handlers,media,label,button,document,documentHandlers,paletteHandlers};
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
test('palette changes apply while motion is paused',()=>{
  const app=setup(true);
  app.paletteHandlers.graphite();
  assert.equal(app.document.body.dataset.theme,'graphite');
  assert.equal(app.frames.size,0);
  app.paletteHandlers.blue();
  assert.equal(app.document.body.dataset.theme,'blue');
});
test('updated reduced-motion preference pauses an existing animation',()=>{
  const app=setup(false);
  app.media.matches=true;app.media.change();
  assert.equal(app.frames.size,0);
  assert.equal(app.label.textContent,'Play animation');
});
