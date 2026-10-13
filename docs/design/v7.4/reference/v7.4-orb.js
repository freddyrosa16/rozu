// rozu v7.4 activity orbs: five orbs instead of ten. Same 3D dot language, projection and morph as v7.3
// (120 points, perspective, depth size/fade, top-left light, depth sort). draw(T) is a pure function of time.
//
// v7.4: every state is a symbol inside the same fully dotted ring, so all ten read at 22px: a turning sphere,
// lines being written, a reading band, a magnifier, a typing caret, a play button, pause bars, a check, a cross
// and a broken ring with a retry arrow.
const N=120,TAU=Math.PI*2,F=3.2;
const cl=(v,a=0,b=1)=>Math.max(a,Math.min(b,v));
const fib=(i,n)=>{const y=1-2*(i+.5)/n,r=Math.sqrt(1-y*y),a=i*2.39996323;return [Math.cos(a)*r,y,Math.sin(a)*r]};
const rX=(p,a)=>{const c=Math.cos(a),s=Math.sin(a);return [p[0],p[1]*c-p[2]*s,p[1]*s+p[2]*c]};
const rY=(p,a)=>{const c=Math.cos(a),s=Math.sin(a);return [p[0]*c+p[2]*s,p[1],-p[0]*s+p[2]*c]};
const hash=i=>{const x=Math.sin(i*127.1+311.7)*43758.5453;return x-Math.floor(x)};
const D=i=>fib(i,N);
const view=(p,t,yaw)=>rX(rY(p,t*yaw),.3);
const sc=(d,r)=>[d[0]*r,d[1]*r,d[2]*r];
const Q=(p,lit,s,a)=>({p,lit,s,a});
// Every orb is the same frame: a fully dotted ring of RING dots (indices 0..RING-1, radius GR) on the plane
// z = GZ facing the camera, around a glyph made of the other GLYPH dots. The ring dots never move between orbs,
// so a change only reshapes the glyph. Both sets are contiguous index runs, so the 22px subsample keeps an even
// share of each.
const RING=44,GLYPH=N-RING,GZ=.3,GR=.86;
const ringAt=j=>{const a=Math.PI/2-TAU*j/RING;return [GR*Math.cos(a),GR*Math.sin(a),GZ]};
const ringAngle=j=>((TAU*j/RING)%TAU); // clockwise from 12 o'clock
// Point at arc-length fraction u along a set of polylines (strokes), as one path.
const along=(strokes,u)=>{const segs=[];let L=0;for(const pts of strokes)for(let k=0;k+1<pts.length;k++){const l=Math.hypot(pts[k+1][0]-pts[k][0],pts[k+1][1]-pts[k][1]);segs.push([pts[k],pts[k+1],l]);L+=l}
  let d=u*L;for(let k=0;k<segs.length;k++){const [a,b,l]=segs[k];if(d<=l||k===segs.length-1){const f=l?cl(d/l):0;return [a[0]+(b[0]-a[0])*f,a[1]+(b[1]-a[1])*f]}d-=l}};
const gu=i=>(i-RING)/(GLYPH-1); // glyph dot i as a fraction 0..1
const ring=(j,lit=0,a=.55)=>Q(ringAt(j),lit,.85+.4*lit,Math.max(a,lit));
const flat=(xy,lit,s,a)=>Q([xy[0],xy[1],GZ],lit,s,a);
// Lag (rad, 0..2π) of ring dot j behind a head moving clockwise at angle h.
const behind=(j,h)=>((h-ringAngle(j))%TAU+TAU)%TAU;
const arc=(c,r,a0,a1,n=24)=>{const p=[];for(let k=0;k<=n;k++){const a=a0+(a1-a0)*k/n;p.push([c[0]+r*Math.cos(a),c[1]+r*Math.sin(a)])}return p};
const CHECK=[[[-.46,.02],[-.14,-.32],[.48,.34]]];
const CROSS=[[[-.36,.36],[.36,-.36]],[[.36,.36],[-.36,-.36]]];
const NOTES=[[[-.44,.26],[.44,.26]],[[-.44,0],[.44,0]],[[-.44,-.26],[.16,-.26]]];
const READ=[[[-.46,.3],[.46,.3]],[[-.46,.1],[.46,.1]],[[-.46,-.1],[.46,-.1]],[[-.46,-.3],[.2,-.3]]];
const PLAY=[[[-.2,.34],[-.2,-.34],[.36,0],[-.2,.34]]];
const PAUSE=[[[-.17,.32],[-.17,-.32]],[[.17,.32],[.17,-.32]]];
const ORBS={
 // THINKING: a small dot sphere turns inside the ring while a lit arc sweeps round the ring.
 thinking:{loop:1.6,gen:(i,t)=>{
   if(i<RING){const g=behind(i,TAU*t/1.6),tail=g<2.2?1-g/2.2:0;return ring(i,tail*tail)}
   const d=fib(i-RING,GLYPH),p=rX(rY(sc(d,.46),t*1.1),.45);
   const lit=cl((p[2]/.46)*.8);return Q(p,lit*.7,.8+.35*lit,.4+.6*lit)}},
 // NOTES: three lines write themselves left to right, then hold and start over.
 notes:{loop:2.6,gen:(i,t)=>{
   if(i<RING)return ring(i);
   const u=gu(i),w=cl((t%2.6)/1.9),on=u<=w,head=on&&u>w-.04&&w<1;
   const xy=along(NOTES,u);return flat(xy,on?1:0,head?1.5:on?1:.45,on?1:.16)}},
 // READING: four lines of text; a lit band moves down the page, one line at a time.
 reading:{loop:2.2,gen:(i,t)=>{
   if(i<RING)return ring(i);
   const xy=along(READ,gu(i)),yb=.42-.84*((t%2.2)/2.2),on=Math.exp(-Math.pow((xy[1]-yb)/.09,2));
   return flat(xy,on,.7+.5*on,.28+.72*on)}},
 // SEARCHING: a magnifying glass circles slowly, scanning.
 searching:{loop:2.4,gen:(i,t)=>{
   if(i<RING)return ring(i);
   const a=TAU*t/2.4,c=[-.08+.12*Math.cos(a),.08+.12*Math.sin(a)];
   const u=gu(i),xy=u<.74?along([arc(c,.24,0,TAU,36)],u/.74):along([[[c[0]+.17,c[1]-.17],[c[0]+.4,c[1]-.4]]],(u-.74)/.26);
   return flat(xy,1,1.05,1)}},
 // EDITING: a line types out behind a blinking caret.
 editing:{loop:2.4,gen:(i,t)=>{
   if(i<RING)return ring(i);
   const ph=t%2.4,w=cl(ph/1.6),x=-.46+.92*w,u=gu(i);
   if(u<.14){const y=-.24+.5*(u/.14),blink=w>=1&&Math.floor((ph-1.6)/.2)%2===1;return flat([x+.05,y],blink?0:1,blink?0:.95,blink?0:1)} // caret
   const v=(u-.14)/.86,xx=-.46+.92*v,on=xx<=x;return flat([xx,-.2],on?1:0,on?1:.45,on?1:.16)}},
 // RUNNING: a play triangle; two lit arcs chase each other round the ring.
 running:{loop:.9,gen:(i,t)=>{
   if(i<RING){let lit=0;for(const off of [0,Math.PI]){const g=behind(i,TAU*t/.9+off);lit=Math.max(lit,g<1.4?1-g/1.4:0)}return ring(i,lit)}
   return flat(along(PLAY,gu(i)),1,1.05,1)}},
 // WAITING (amber): pause bars; the whole ring breathes, asking for you.
 waiting:{loop:1.8,tone:"warning",gen:(i,t)=>{
   const b=.5+.5*Math.sin(TAU*t/1.8);
   if(i<RING)return ring(i,.25+.55*b);
   return flat(along(PAUSE,gu(i)),1,1.05+.15*b,1)}},
 // DONE (green): a check mark draws itself once, then holds.
 done:{loop:0,tone:"success",gen:(i,t)=>{
   if(i<RING)return ring(i);
   const u=gu(i),xy=along(CHECK,u),drawn=cl((t-.25)/.45),on=u<=drawn;
   const pop=t>.7&&t<.95?Math.sin(Math.PI*(t-.7)/.25):0;
   return flat(xy,on?1:.25,(on?1.15:.6)*(1+.2*pop),on?1:.35)}},
 // ERROR (red): a cross that shakes once, like a head saying no.
 error:{loop:0,tone:"danger",gen:(i,t)=>{
   const st=t-.3,dx=st>0&&st<.9?.13*Math.exp(-5*st)*Math.sin(28*st):0;
   if(i<RING){const r=ring(i);r.p[0]+=dx;return r}
   const xy=along(CROSS,gu(i));return flat([xy[0]+dx,xy[1]],1,1.15,1)}},
 // OFFLINE (dim): the ring is broken at the top; a retry arrow inside turns slowly.
 offline:{loop:2.4,dim:true,gen:(i,t)=>{
   if(i<RING){const a=ringAngle(i),gap=a<.55||a>TAU-.55;return gap?Q(ringAt(i),0,0,0):ring(i,0,.4)}
   const u=gu(i),rot=-TAU*t/2.4;
   if(u<.8){const a=rot+Math.PI*.5+1.6*Math.PI*(u/.8);return flat([.4*Math.cos(a),.4*Math.sin(a)],.7,1.05,.9)}
   const tip=rot+Math.PI*.5+1.6*Math.PI,v=(u-.8)/.2,side=v<.5?1:-1,k=.24*(v<.5?v:v-.5)*2;
   const back=tip-Math.PI*.5+side*.8; // the arrow travels anticlockwise; its barbs point back from the tip
   return flat([.4*Math.cos(tip)+k*Math.cos(back),.4*Math.sin(tip)+k*Math.sin(back)],.7,1.05,.9)}},
};
const ORDER=["thinking","notes","reading","searching","editing","running","waiting","done","error","offline"];
const STILL={thinking:.55,notes:1.2,reading:.6,searching:.3,editing:1.1,running:.3,waiting:.45,done:1.5,error:1.5,offline:.3};
const MORPH=.6,STAG=.0025;
const crit=u=>u<=0?0:u>=1.6?1:1-(1+7*u)*Math.exp(-7*u);
// tl: [{orb, start}], consecutive entries with different orbs.
function sample(tl,T){let k=0;while(k+1<tl.length&&tl[k+1].start<=T)k++;const cur=tl[k],tin=T-cur.start,S=ORBS[cur.orb];
  const out=[];for(let i=0;i<N;i++){const b=S.gen(i,tin);
    if(k===0){out.push({...b,tone:S.tone,dim:S.dim});continue}
    const pv=tl[k-1],P=ORBS[pv.orb],a=P.gen(i,T-pv.start),q=crit((tin-i*STAG)/MORPH),lift=.55*Math.sin(Math.PI*q);
    out.push({p:[a.p[0]+(b.p[0]-a.p[0])*q,a.p[1]+(b.p[1]-a.p[1])*q,a.p[2]+(b.p[2]-a.p[2])*q+lift],lit:cl(a.lit+(b.lit-a.lit)*q+(tin<.15?.8*Math.sin(Math.PI*tin/.15):0)),s:a.s+(b.s-a.s)*q,
      a:a.a+(b.a-a.a)*q,tone:q>.5?S.tone:P.tone,dim:q>.5?S.dim:P.dim})}return out}
function css(v){return getComputedStyle(document.body).getPropertyValue(v).trim()}
function drawOrb(canvas,tl,T){const dpr=(window.devicePixelRatio||1),size=canvas.clientWidth;canvas.width=Math.round(size*dpr);canvas.height=Math.round(size*dpr);
  const ctx=canvas.getContext('2d');ctx.scale(dpr,dpr);ctx.clearRect(0,0,size,size);
  const small=size<=30,R=size*.4,base=small?size*.05:size*.017,px=1/dpr;
  const col={none:css('--orb-dot'),warning:css('--warning'),success:css('--success'),danger:css('--danger')};
  let pts=sample(tl,T);if(small)pts=pts.filter((_,i)=>((i>>1)+i)%2===0);
  const pr=pts.map(b=>{const z=b.p[2],k=F/(F-z),dn=cl((z+1.2)/2.4);return {...b,x:size/2+b.p[0]*k*R,y:size/2-b.p[1]*k*R,z,k,dn}}).sort((a,b)=>a.z-b.z);
  for(const b of pr){if(b.s<=.02||b.k<=.02)continue;const ln=Math.hypot(...b.p)||1,lam=(-.55*b.p[0]+.6*b.p[1]+.58*b.p[2])/ln*.5+.5;let r=base*b.s*b.k*(0.4+0.6*b.dn)*(.9+.2*lam)*(1+.55*b.lit);r=Math.max(px*.75,r);
    let a=(.12+.88*b.dn)*(.62+.48*lam);a=cl(a+.5*b.lit)*b.a;if(b.dim)a*=.85;ctx.globalAlpha=cl(a);ctx.fillStyle=col[b.tone||'none'];
    if(small&&r*dpr<1.6){ctx.fillRect(Math.round((b.x-r)*dpr)/dpr,Math.round((b.y-r)*dpr)/dpr,Math.max(px,Math.round(2*r*dpr)/dpr),Math.max(px,Math.round(2*r*dpr)/dpr))}
    else{ctx.beginPath();ctx.arc(b.x,b.y,r,0,TAU);ctx.fill()}}
  ctx.globalAlpha=1}
if(typeof module!=="undefined")module.exports={N,ORBS,ORDER,STILL,sample,hash,fib};
