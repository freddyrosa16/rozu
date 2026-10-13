// rozu v7.2 "flow orbs": a family of 3D dot orbs. Each state is a real 3D point set (N=120, ≤200), rotated in 3D,
// projected with perspective, sized/faded by depth and depth-sorted. Deterministic: draw(T) is a pure function of time.
const N=120,TAU=Math.PI*2,F=3.2; // F = camera distance (unit-radius orbs)
const cl=(v,a=0,b=1)=>Math.max(a,Math.min(b,v));
const fib=(i,n)=>{const y=1-2*(i+.5)/n,r=Math.sqrt(1-y*y),a=i*2.39996323;return [Math.cos(a)*r,y,Math.sin(a)*r]};
const rX=(p,a)=>{const c=Math.cos(a),s=Math.sin(a);return [p[0],p[1]*c-p[2]*s,p[1]*s+p[2]*c]};
const rY=(p,a)=>{const c=Math.cos(a),s=Math.sin(a);return [p[0]*c+p[2]*s,p[1],-p[0]*s+p[2]*c]};
const rZ=(p,a)=>{const c=Math.cos(a),s=Math.sin(a);return [p[0]*c-p[1]*s,p[0]*s+p[1]*c,p[2]]};
const hash=i=>{const x=Math.sin(i*127.1+311.7)*43758.5453;return x-Math.floor(x)};
const sprO=u=>u<=0?0:u>=1.6?1:1-Math.exp(-6*u)*(Math.cos(9*u)+.65*Math.sin(9*u)); // elastic, ζ≈.55
const Q=(p,lit=0,s=1)=>({p,lit,s});
// v7.1: ONE visual language. Every state is the same spherical cloud of 120 dots with radial flow
// (Fibonacci-sphere directions d_i, per-dot phase φ_i). States differ only in how the flow behaves.
const D=i=>fib(i,N),H=i=>hash(i);
const view=(p,t,yaw=.5)=>rX(rY(p,t*yaw),.3);
const sc=(d,r)=>[d[0]*r,d[1]*r,d[2]*r];
const ease=x=>x*x;
const C=(p,on,so,sf,af)=>{const q=Q(p,on?1:0,on?so:sf);q.a=on?1:af;return q};
const STATES={
 thinking:{label:"Thinking…",loop:1.6,gen:(i,t)=>{ // CONVERGE + BRIGHT DENSE CORE: dots rush in then linger at the core (φ^.55), core dots lit
   const ph=(t/1.6+H(i))%1,r=1-.88*Math.pow(ph,.55);const q=Q(view(sc(D(i),r),t),r<.42?1:0,r<.42?1.3:.6);q.a=r<.42?1:.4;return q}},
 searching:{label:"Searching \"set_size\"",loop:2.4,gen:(i,t)=>{ // SCAN LENS: a bright lit cap orbits the rim like a lens sweeping; the rest flows in, faded
   const ph=(t/2.4+H(i))%1,p=view(sc(D(i),1-.5*ph*ph),t,.2),a=TAU*t/2.4,c0=[.78*Math.cos(a),.5*Math.sin(a),.45],cm=Math.hypot(...c0),c=c0.map(v=>v/cm),m=Math.hypot(...p)||1,
   on=(p[0]*c[0]+p[1]*c[1]+p[2]*c[2])/m>.86;if(on){const k=1.06;p[0]*=k;p[1]*=k}return C(p,on,1.35,.5,.25)}},
 reading:{label:"Reading README.md",loop:2.2,gen:(i,t)=>{ // TILTED LIT BAND: a diagonal band (screen-space axis) lit across a gently inflowing cloud, sliding slowly
   const ph=(t/2.2+H(i))%1,p=view(sc(D(i),1-.18*ph),t,.25),c=.25*Math.sin(TAU*t/4.4),v=.75*p[1]-.66*p[0],on=Math.abs(v-c)<.17;
   const q=Q(p,on?1:0,on?1.2:.7);q.a=on?1:.55;return q}},
 notes:{label:"Jotting down notes…",loop:3.0,gen:(i,t)=>{ // INK STROKE: a lit comet stroke writes round a tilted latitude, tail fading; the cloud drifts out, faded
   const d=D(i),ph=(t/3+H(i))%1,p=view(sc(d,.7+.3*ph),t,.15),lon=Math.atan2(d[2],d[0]),lag=((TAU*t/1.5-lon)%TAU+TAU)%TAU,band=Math.abs(d[1]-.45*Math.sin(lon))<.26,
   on=band&&lag<4.4;const q=C(p,on,1.25-.3*lag/4.4,.45,.18);if(on)q.a=1-.35*lag/4.4;return q}},
 editing:{label:"Editing src/term/view.rs",loop:1.06,gen:(i,t)=>{ // TIGHT BALL WITH A GAP: compressed ball (r≈.5) clicking inward every 530ms; a caret-shaped wedge is empty
   const u=(t%.53)/.53,pulse=u<.12?u/.12:Math.exp(-(u-.12)*9),ph=(t/2.4+H(i))%1,d=D(i),p=view(sc(d,(.66-.08*ph)*(1-.22*pulse)),0,0);
   const gap=Math.abs(Math.atan2(p[1],p[0]))<.75;return Q(p,pulse>.5?.8:.2,gap?0:1.15)}},
 running:{label:"Running cargo test",loop:.8,gen:(i,t)=>{ // 2 SPIRAL ARMS: fast inflow; twist grows with depth into the core so two lit arms wind into spirals
   const ph=(t/.8+H(i))%1,d=D(i),arm=Math.cos(2*Math.atan2(d[2],d[0]))>.55,r=1-.86*ph,p=rX(rY(sc(d,r),2.6*ph+t*2.5),1.15);
   const q=Q(p,arm?1:0,arm?1.35:.45);q.a=arm?1:.18;return q}},
 waiting:{label:"Waiting for approval",loop:2.6,tone:"warning",gen:(i,t)=>{ // PAUSE BARS: two lit vertical bands on a frozen cloud that breathes
   const ph=H(i)*.85,b=Math.sin(TAU*t/2.6),p=view(sc(D(i),(1-.5*ph*ph)*(1+.06*b)),0,0),on=Math.abs(Math.abs(p[0])-.32)<.12&&Math.abs(p[1])<.7;
   const q=C(p,on,1.3,.5,.22);if(on)q.lit=.7+.3*(b*.5+.5);return q}},
 done:{label:"Done · 42s · 2 files",loop:0,tone:"success",gen:(i,t)=>{ // HALO: the cloud settles on the shell; its silhouette rim lights as a bright halo after one outward ripple
   const d=D(i),front=1-2*cl(t/1.1),rip=t<1.3?.12*Math.exp(-Math.pow((d[1]-front)/.18,2)):0,p=view(sc(d,.84+rip),t,.12),m=Math.hypot(...p)||1,on=1-Math.abs(p[2])/m>.6;
   return C(p,on||rip>.06,1.3,.55,.3)}},
 error:{label:"Command failed · R retry",loop:0,tone:"danger",gen:(i,t)=>{ // BREAK: the flow stalls, then a lit third is flung out once into a ragged outer ring; the core stays dim
   const d=D(i),ph=H(i)*.85,r0=.55-.3*ph*ph,out=H(i+7)<.38,sct=t<.18?0:sprO((t-.18)/.5);
   const p=view(sc(d,out?r0+(.6+.15*H(i+3))*sct:r0*(1-.1*sct)),0,0);return C(p,out,1.3,.55,.28)}},
 offline:{label:"Offline · retrying in 3s",loop:2.4,dim:true,gen:(i,t)=>{ // BROKEN RING: a lit equator ring with a dark gap; a dim spark sputters across the gap; the rest is a faint sparse cloud
   const d=D(i),k=Math.floor(t/.3),ph=((k+((k%4)<2?(t%.3)/.3:1))*.07+H(i))%1,p=view(sc(d,1-.4*ph*ph),t,.18),lon=Math.atan2(p[2],p[0]),
   ring=Math.abs(p[0])<.15,gap=p[1]>-.05&&p[1]<.5,spark=gap&&ring&&(k%4===1);
   if(!ring&&i%3)return Q(p,0,0);return C(p,ring&&(!gap||spark),1.25,.5,.2)}},
};
const ORDER=["thinking","notes","reading","searching","editing","running","waiting","done","error","offline"];
const MORPH=.6,STAG=.0025;
const crit=u=>u<=0?0:u>=1.6?1:1-(1+7*u)*Math.exp(-7*u);
function sample(tl,T){let k=0;while(k+1<tl.length&&tl[k+1].start<=T)k++;const cur=tl[k],tin=T-cur.start,S=STATES[cur.state];
  const out=[];for(let i=0;i<N;i++){const b=S.gen(i,tin);
    if(k===0){out.push({...b,tone:S.tone,dim:S.dim});continue}
    const pv=tl[k-1],a=STATES[pv.state].gen(i,T-pv.start),q=crit((tin-i*STAG)/MORPH),lift=.55*Math.sin(Math.PI*q); // lift toward camera mid-morph
    out.push({p:[a.p[0]+(b.p[0]-a.p[0])*q,a.p[1]+(b.p[1]-a.p[1])*q,a.p[2]+(b.p[2]-a.p[2])*q+lift],lit:cl(a.lit+(b.lit-a.lit)*q+(tin<.15?.8*Math.sin(Math.PI*tin/.15):0)),s:a.s+(b.s-a.s)*q,
      a:(a.a??1)+((b.a??1)-(a.a??1))*q,lit2:0,tone:q>.5?S.tone:STATES[pv.state].tone,dim:q>.5?S.dim:STATES[pv.state].dim})}return out}
function css(v){return getComputedStyle(document.body).getPropertyValue(v).trim()}
function drawOrb(canvas,tl,T){const dpr=(window.devicePixelRatio||1),size=canvas.clientWidth;canvas.width=Math.round(size*dpr);canvas.height=Math.round(size*dpr);
  const ctx=canvas.getContext('2d');ctx.scale(dpr,dpr);ctx.clearRect(0,0,size,size);
  const small=size<=30,R=size*.4,base=small?size*.05:size*.017,px=1/dpr;
  const col={none:css('--orb-dot'),warning:css('--warning'),success:css('--success'),danger:css('--danger')};
  let pts=sample(tl,T);if(small)pts=pts.filter((_,i)=>((i>>1)+i)%2===0); // keeps both parities, so alternating-bar states keep both bars // 22px optical size: 60 dots, larger
  const pr=pts.map(b=>{const z=b.p[2],s=F/(F-z),dn=cl((z+1.2)/2.4);return {...b,x:size/2+b.p[0]*s*R,y:size/2-b.p[1]*s*R,z,k:s,dn}}).sort((a,b)=>a.z-b.z);
  for(const b of pr){if(b.s<=.02||b.k<=.02)continue;const ln=Math.hypot(...b.p)||1,lam=(-.55*b.p[0]+.6*b.p[1]+.58*b.p[2])/ln*.5+.5;let r=base*b.s*b.k*(0.4+0.6*b.dn)*(.9+.2*lam)*(1+.55*b.lit)*(F/(F-0))/1;r=Math.max(px*.75,r);
    let a=(.12+.88*b.dn)*(.62+.48*lam);a=cl(a+.5*b.lit)*(b.a??1);if(b.dim)a*=.85;ctx.globalAlpha=a;ctx.fillStyle=col[b.tone||'none'];
    if(small&&r*dpr<1.6){ctx.fillRect(Math.round((b.x-r)*dpr)/dpr,Math.round((b.y-r)*dpr)/dpr,Math.max(px,Math.round(2*r*dpr)/dpr),Math.max(px,Math.round(2*r*dpr)/dpr))} // ≤1.6 device-px dots: crisp pixel squares
    else{ctx.beginPath();ctx.arc(b.x,b.y,r,0,TAU);ctx.fill()}}
  ctx.globalAlpha=1}
