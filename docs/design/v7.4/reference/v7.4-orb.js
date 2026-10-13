// rozu v7.4 activity orbs: five orbs instead of ten. Same 3D dot language, projection and morph as v7.3
// (120 points, perspective, depth size/fade, top-left light, depth sort). draw(T) is a pure function of time.
//
// v7.4 maps the ten activity states onto five orbs. Every "busy" state (thinking, notes, reading, searching,
// editing, running) shares the Working orb, because the label already says what the agent is doing, and a
// different orb per tool made a fast agent loop morph several times a second. The four that need a different
// read at a glance keep their own orb: Waiting (needs you), Done, Error and Offline.
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
// Glyph layout shared by Done, Error and Offline: a faint ring of RING dots (indices 0..RING-1) around a bold
// stroke glyph made of the rest, drawn on a plane at z = GZ facing the camera. Both sets are contiguous index
// runs, so the 22px subsample keeps an even share of each.
const RING=44,GZ=.3,GR=.86;
const ringAt=(j,n,rot)=>{const a=TAU*j/n+rot;return [GR*Math.cos(a),GR*Math.sin(a),GZ]};
// Point at arc-length fraction u along a polyline.
const along=(pts,u)=>{const seg=[];let L=0;for(let k=0;k+1<pts.length;k++){const l=Math.hypot(pts[k+1][0]-pts[k][0],pts[k+1][1]-pts[k][1]);seg.push(l);L+=l}
  let d=u*L;for(let k=0;k<seg.length;k++){if(d<=seg[k]||k===seg.length-1){const f=seg[k]?cl(d/seg[k]):0;return [pts[k][0]+(pts[k+1][0]-pts[k][0])*f,pts[k][1]+(pts[k+1][1]-pts[k][1])*f]}d-=seg[k]}};
const CHECK=[[-.46,.02],[-.14,-.32],[.48,.34]];
const CROSS_A=[[-.36,.36],[.36,-.36]],CROSS_B=[[.36,.36],[-.36,-.36]];
const ORBS={
 // WORKING: a slowly turning sphere with one lit comet orbiting it on a tilted path, its tail fading. Reads as a
 // spinner at 22px (a bright arc going round) and as a 3D orbit at 88px (the comet grows in front, shrinks behind).
 working:{loop:1.4,gen:(i,t)=>{
   const d=D(i),r=.9+.05*Math.sin(TAU*t/2.8+3*d[1]),p=view(sc(d,r),t,.35),m=Math.hypot(...p)||1;
   const q=rX([p[0]/m,p[1]/m,p[2]/m],-.75); // orbit frame: the orbit is q.y≈0, tilted toward the camera
   const th=Math.atan2(q[2],q[0]),head=TAU*t/1.4,band=Math.exp(-Math.pow(q[1]/.5,2));
   let lit=0;for(const off of [0,Math.PI]){const lag=((head+off-th)%TAU+TAU)%TAU,tail=lag<2.4?1-lag/2.4:0;lit=Math.max(lit,tail*Math.sqrt(tail)*(off?.55:1))} // a comet and a fainter twin opposite it
   lit=cl(1.35*band*lit);return Q(p,lit,.6+.8*lit,.38+.62*lit)}},
 // WAITING (amber): the sphere stops turning and breathes; a lit ring pings outward from its face every 1.8s,
 // like a notification asking for you.
 waiting:{loop:1.8,tone:"warning",gen:(i,t)=>{
   const d=D(i),b=Math.sin(TAU*t/1.8),p=view(sc(d,.86*(1+.04*b)),0,0),m=Math.hypot(...p)||1;
   const g=Math.acos(cl(p[2]/m,-1,1)),u=(t/1.8)%1,front=u*2.2,ping=Math.exp(-Math.pow((g-front)/.28,2))*(1-u);
   const lit=cl(ping*1.4),glow=.25*(b*.5+.5)*Math.max(0,p[2]/m); // the face glows faintly with the breath between pings
   return Q(p,lit,.5+.8*lit,.32+.68*Math.max(lit,glow))}},
 // DONE (green): the cloud gathers into a check mark inside a faint ring; the check draws itself once, then holds.
 done:{loop:0,tone:"success",gen:(i,t)=>{
   if(i<RING)return Q(ringAt(i,RING,Math.PI/2),0,.5,.3);
   const u=(i-RING)/(N-RING-1),xy=along(CHECK,u),drawn=cl((t-.25)/.45),on=u<=drawn;
   const pop=t>.7&&t<.95?Math.sin(Math.PI*(t-.7)/.25):0;
   return Q([xy[0],xy[1],GZ],on?1:.25,(on?1.15:.6)*(1+.2*pop),on?1:.35)}},
 // ERROR (red): the cloud gathers into a cross inside a faint ring, which shakes once like a head saying no.
 error:{loop:0,tone:"danger",gen:(i,t)=>{
   const st=t-.3,dx=st>0&&st<.9?.13*Math.exp(-5*st)*Math.sin(28*st):0;
   if(i<RING){const r=ringAt(i,RING,Math.PI/2);return Q([r[0]+dx,r[1],r[2]],0,.5,.3)}
   const j=i-RING,n=N-RING,half=n>>1,u=j<half?j/(half-1):(j-half)/(n-half-1),xy=along(j<half?CROSS_A:CROSS_B,u);
   return Q([xy[0]+dx,xy[1],GZ],1,1.15,1)}},
 // OFFLINE (dim): a dashed ring turning slowly, the way a connection retries; nothing inside it.
 offline:{loop:2.4,dim:true,gen:(i,t)=>{
   const a=TAU*i/N-TAU*t/2.4,seg=((i/N*6)%1),gap=seg>.62;
   const r=[GR*Math.cos(a+Math.PI/2),GR*Math.sin(a+Math.PI/2),GZ];
   if(gap)return Q(r,0,0,0);
   const head=Math.abs(seg-.31)<.08;return Q(r,head?.6:0,head?.9:.65,head?.8:.45)}},
};
// Which orb each activity state shows.
const ORB_OF={thinking:"working",notes:"working",reading:"working",searching:"working",editing:"working",running:"working",
  waiting:"waiting",done:"done",error:"error",offline:"offline"};
const ORBS_ORDER=["working","waiting","done","error","offline"];
const STILL={working:.45,waiting:.5,done:1.5,error:1.5,offline:0};
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
if(typeof module!=="undefined")module.exports={N,ORBS,ORB_OF,ORBS_ORDER,STILL,sample,hash,fib};
