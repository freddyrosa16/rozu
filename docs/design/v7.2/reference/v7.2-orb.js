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
const STATES={
 thinking:{label:"Thinking…",loop:1.6,gen:(i,t)=>{ // CONVERGE + BRIGHT DENSE CORE: dots rush in then linger at the core (φ^.55), core dots lit
   const ph=(t/1.6+H(i))%1,r=1-.88*Math.pow(ph,.55);const q=Q(view(sc(D(i),r),t),r<.42?1:0,r<.42?1.3:.6);q.a=r<.42?1:.4;return q}},
 searching:{label:"Searching \"set_size\"",loop:2.4,gen:(i,t)=>{ // OFF-CENTRE HOT SPOT: the stream converges on a focal point parked upper-right that drifts slowly
   const ph=(t/2.4+H(i))%1,d=D(i),f=[.78+.05*Math.cos(TAU*t/2.4),.62+.05*Math.sin(TAU*t/2.4),.1],e=.96*Math.pow(ph,.6);
   const p=[d[0]+(f[0]-d[0])*e,d[1]+(f[1]-d[1])*e,d[2]+(f[2]-d[2])*e];const q=Q(rX(p,.15),e>.6?1:0,e>.6?1.3:.55);q.a=e>.6?1:.3;return q}},
 reading:{label:"Reading README.md",loop:2.2,gen:(i,t)=>{ // TILTED LIT BAND: a diagonal band (screen-space axis) lit across a gently inflowing cloud, sliding slowly
   const ph=(t/2.2+H(i))%1,p=view(sc(D(i),1-.18*ph),t,.25),c=.25*Math.sin(TAU*t/4.4),v=.75*p[1]-.66*p[0],on=Math.abs(v-c)<.17;
   const q=Q(p,on?1:0,on?1.2:.7);q.a=on?1:.55;return q}},
 notes:{label:"Jotting down notes…",loop:3.6,gen:(i,t)=>{ // HOLLOW CENTRE, BRIGHT RIM: slow outward flow on the shell; silhouette (fresnel) dots lit, face-on dots faint
   const ph=(t/3.6+H(i))%1,p=view(sc(D(i),.78+.22*ph),t,.3),rim=1-Math.abs(p[2])/Math.hypot(p[0],p[1],p[2]),on=rim>.55;
   const q=Q(p,on?1:0,on?1.2:.6);q.a=on?1:.3;return q}},
 editing:{label:"Editing src/term/view.rs",loop:1.06,gen:(i,t)=>{ // TIGHT BALL WITH A GAP: compressed ball (r≈.5) clicking inward every 530ms; a caret-shaped wedge is empty
   const u=(t%.53)/.53,pulse=u<.12?u/.12:Math.exp(-(u-.12)*9),ph=(t/2.4+H(i))%1,d=D(i),p=view(sc(d,(.66-.08*ph)*(1-.22*pulse)),0,0);
   const gap=Math.abs(Math.atan2(p[1],p[0]))<.75;return Q(p,pulse>.5?.8:.2,gap?0:1.15)}},
 running:{label:"Running cargo test",loop:.8,gen:(i,t)=>{ // 2 SPIRAL ARMS: fast inflow; twist grows with depth into the core so two lit arms wind into spirals
   const ph=(t/.8+H(i))%1,d=D(i),arm=Math.cos(2*Math.atan2(d[2],d[0]))>.55,r=1-.86*ph,p=rX(rY(sc(d,r),2.6*ph+t*2.5),1.15);
   const q=Q(p,arm?1:0,arm?1.35:.45);q.a=arm?1:.18;return q}},
 waiting:{label:"Waiting for approval",loop:2.6,tone:"warning",gen:(i,t)=>{ // PAUSED: the flow freezes mid-stream; the frozen cloud breathes
   const ph=H(i)*.85,b=Math.sin(TAU*t/2.6),r=(1-.86*ease(ph))*(1+.07*b);return Q(view(sc(D(i),r),t,.08),.35+.4*(b*.5+.5),.7+.5*ph)}},
 done:{label:"Done · 42s · 2 files",loop:0,tone:"success",gen:(i,t)=>{ // SETTLE: every dot settles onto the shell (calm, still sphere); ONE outward ripple travels pole to pole
   const d=D(i),front=1-2*cl(t/1.1),rip=t<1.3?.13*Math.exp(-Math.pow((d[1]-front)/.18,2)):0;return Q(view(sc(d,.82+rip),t,.15),1,1)}},
 error:{label:"Command failed · R retry",loop:0,tone:"danger",gen:(i,t)=>{ // STALL + SCATTER: flow stalls (dots freeze), then scatters outward once and holds, uneven
   const ph=H(i)*.85,r0=1-.86*ease(ph),sct=t<.18?0:sprO((t-.18)/.5),j=H(i+57)*2-1,d=D(i);
   const p=sc([d[0]+.25*j*d[2],d[1]+.25*H(i+11)-.12,d[2]-.25*j*d[0]],r0+(.28+.25*H(i+3))*sct);return Q(view(p,0,0),1,.85)}},
 offline:{label:"Offline · retrying in 3s",loop:2.4,dim:true,gen:(i,t)=>{ // SPUTTER: a sparse third of the dots; the flow advances in stuttering bursts and stalls
   if(i%3)return Q(view(sc(D(i),.5),t,.2),0,0);const k=Math.floor(t/.3),burst=(k%4)<2?((t%.3)/.3):1,ph=((k+burst)*.07+H(i))%1;
   return Q(view(sc(D(i),1-.86*ease(ph)),t,.2),ph>.75?.6:0,.8+.4*ph)}},
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
  const pr=pts.map(b=>{const z=b.p[2],s=F/(F-z),dn=cl((z+1.2)/2.4);return {...b,x:size/2+b.p[0]*s*R,y:size/2-b.p[1]*s*R,z,s,dn}}).sort((a,b)=>a.z-b.z);
  for(const b of pr){if(b.s<=.02)continue;const ln=Math.hypot(...b.p)||1,lam=(-.55*b.p[0]+.6*b.p[1]+.58*b.p[2])/ln*.5+.5;let r=base*b.s*(0.4+0.6*b.dn)*(.9+.2*lam)*(1+.55*b.lit)*(F/(F-0))/1;r=Math.max(px*.75,r);
    let a=(.12+.88*b.dn)*(.62+.48*lam);a=cl(a+.5*b.lit)*(b.a??1);if(b.dim)a*=.85;ctx.globalAlpha=a;ctx.fillStyle=col[b.tone||'none'];
    if(small&&r*dpr<1.6){ctx.fillRect(Math.round((b.x-r)*dpr)/dpr,Math.round((b.y-r)*dpr)/dpr,Math.max(px,Math.round(2*r*dpr)/dpr),Math.max(px,Math.round(2*r*dpr)/dpr))} // ≤1.6 device-px dots: crisp pixel squares
    else{ctx.beginPath();ctx.arc(b.x,b.y,r,0,TAU);ctx.fill()}}
  ctx.globalAlpha=1}
