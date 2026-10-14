// rozu v7.5 activity orbs: a dotted globe for every state.
//
// Every state is the same object, a sphere of small matte dots laid out on lines of latitude, and the state
// is what the globe is doing: sparks rippling over it (thinking), a reading band descending it (reading), a lens
// roaming its surface (searching), a caret writing every dot from the bottom to the top (editing), a fast helical spin (running), a slow breath (waiting), a flicker of dropped rings
// (offline). Done fills the globe up green and pops; Error is the full red globe with one shake; both then hold. Same perspective, light, depth and morph as v7.3/v7.4.
//
// Two sizes, tuned separately, not scaled: LARGE (≥ 30px, 15 rings) and SMALL (the 22px pill, 9 rings). Each
// is its own point set, so there is no subsample any more. gen(i, t, P) is a pure function of time.
const TAU=Math.PI*2,F=3.2;
const cl=(v,a=0,b=1)=>Math.max(a,Math.min(b,v));
const rX=(p,a)=>{const c=Math.cos(a),s=Math.sin(a);return [p[0],p[1]*c-p[2]*s,p[1]*s+p[2]*c]};
const rY=(p,a)=>{const c=Math.cos(a),s=Math.sin(a);return [p[0]*c+p[2]*s,p[1],-p[0]*s+p[2]*c]};
const hash=i=>{const x=Math.sin(i*127.1+311.7)*43758.5453;return x-Math.floor(x)};
const hash2=(a,b)=>{const x=Math.sin(a*12.9898+b*78.233)*43758.5453;return x-Math.floor(x)};
// Smooth value noise on a 2D lattice; deterministic, Math-only.
const vnoise=(x,y)=>{const xi=Math.floor(x),yi=Math.floor(y);let fx=x-xi,fy=y-yi;fx=fx*fx*(3-2*fx);fy=fy*fy*(3-2*fy);
  const a=hash2(xi,yi),b=hash2(xi+1,yi),c=hash2(xi,yi+1),d=hash2(xi+1,yi+1);return a+(b-a)*fx+(c-a)*fy+(a-b-c+d)*fx*fy};
const Q=(p,lit,s,a)=>({p,lit,s,a});
const TILT=.3,R=.9;
// A size profile: `rings` lines of latitude, up to `lon` dots round the equator (fewer toward the poles so the
// dots stay evenly spaced), and `grid[i] = [phi, lambda, ring]` for every dot. N = grid.length.
function profile(rings,lon,tune){const grid=[];for(let j=0;j<rings;j++){const phi=-Math.PI/2+Math.PI*(j+.5)/rings,n=Math.max(3,Math.round(lon*Math.cos(phi)));
  for(let k=0;k<n;k++)grid.push([phi,TAU*k/n,j])}return {rings,lon,grid,N:grid.length,...tune}}
const LARGE=profile(15,30,{dot:1,speed:1});   // 286 dots
const SMALL=profile(9,15,{dot:1.15,speed:1.15}); // 87 dots
// Dot i's unit direction before the spin.
const dir=(P,i)=>{const [phi,lam]=P.grid[i];const c=Math.cos(phi);return [c*Math.cos(lam),Math.sin(phi),c*Math.sin(lam)]};
// Dot i on the globe, radius r, spun by `yaw` radians and tilted toward the camera.
const globe=(P,i,yaw,r=R,tilt=TILT)=>{const d=dir(P,i);return rX(rY([d[0]*r,d[1]*r,d[2]*r],yaw),tilt)};
// Fraction 0..1 round a ring, starting at the front and going round the back (writing order).
const roundFrom=(P,i)=>((-front(P,i,0))%TAU+TAU)%TAU/TAU;
// Geodesic angle (rad) between dot i and a surface point (phi0, lam0).
const geo=(P,i,phi0,lam0)=>{const [phi,lam]=P.grid[i];return Math.acos(cl(Math.sin(phi)*Math.sin(phi0)+Math.cos(phi)*Math.cos(phi0)*Math.cos(lam-lam0),-1,1))};
// Where dot i faces the camera: 0 at the front, ±π at the back (after a spin of `yaw`).
const front=(P,i,yaw)=>{const a=P.grid[i][1]-yaw-Math.PI/2;return Math.atan2(Math.sin(a),Math.cos(a))};
// The breathing globe at breath b (0 small and dim, 1 full and bright): Waiting, and what Done and
// Error become after Waiting (the breath fills up and holds, in green or red, instead of a morph).
const breath=(P,i,b)=>{const p=globe(P,i,0,R*(.96+.06*b));return Q(p,.2+.5*b,(.9+.25*b)*P.dot,.6+.4*b)};
const ORBS={
 thinking:{tone:"none",tempo:1.6,gen(i,t,P){ // thought weather: patches of light drift over a slowly turning globe
   // Two sparks take turns: each flashes at a point on the surface, and a ripple spreads out from it and fades.
   const yaw=.35*t*P.speed,p=globe(P,i,yaw);let lit=0;
   for(let k=0;k<2;k++){const T=1.6,a=((t+k*T/2)%T),c=Math.floor((t+k*T/2)/T),h1=hash2(c,k+1),h2=hash2(k+1,c);
     const d=geo(P,i,(2*h1-1)*.6,yaw+Math.PI/2+(2*h2-1)*1.1),ring=Math.exp(-Math.pow((d-a*1.1)/.3,2))*Math.pow(1-a/T,1.5),spark=d<.25?Math.max(0,1-a/.4):0;
     lit=Math.max(lit,ring,spark)}
   return Q(p,lit,(.85+.45*lit)*P.dot,.45+.55*lit)}},
 reading:{tone:"none",tempo:2.2,gen(i,t,P){ // a lit band reads down the globe, line by line
   const [phi]=P.grid[i],p=globe(P,i,.25*t*P.speed),yb=Math.PI/2-Math.PI*((t%2.2)/2.2)*1.15+.07,on=Math.exp(-Math.pow((phi-yb)/.17,2));
   return Q(p,on,(.8+.5*on)*P.dot,.35+.65*on)}},
 searching:{tone:"none",tempo:2.4,gen(i,t,P){ // a lens roams the surface; the dots under it swell
   const [phi,lam]=P.grid[i],yaw=.3*t*P.speed,p=globe(P,i,yaw),a=TAU*t/2.4,cphi=.35*Math.sin(a*.7),clam=front(P,i,yaw)-(.9*Math.sin(a));
   const d=Math.hypot(phi-cphi,clam*Math.cos(cphi)),inside=cl((.42-d)/.12),lit=inside;
   return Q(p,lit,(.85+.55*lit)*P.dot,.4+.6*lit)}},
 editing:{tone:"none",tempo:2.6,gen(i,t,P){ // a caret writes every dot, ring by ring, from the bottom of the globe to the top
   const [phi,lam,j]=P.grid[i],p=globe(P,i,0),ph=t%2.6,u=roundFrom(P,i),o=(j+u)/P.rings,w=Math.min(1,ph/2); // written over 2s, held, cleared over the last .3s
   if(ph>=2.3){const q=(ph-2.3)/.3;return Q(p,.75*(1-q),(1.15-.4*q)*P.dot,.9-.6*q)}
   if(o>=w)return Q(p,0,.75*P.dot,.3);
   const f=Math.max(0,1-(w-o)*P.rings/1.5),head=ph<2&&(w-o)*P.rings*Math.max(3,Math.round(P.lon*Math.cos(phi)))<1; // the last ring and a half is fresh; the newest dot is the caret
   return Q(p,head?1:.75+.25*f,(head?1.7:1.15+.5*f)*P.dot,.9+.1*f)}},
 running:{tone:"none",tempo:.9,gen(i,t,P){ // the globe spins fast with a helical stripe racing round it
   const [phi,lam]=P.grid[i],yaw=2.4*t*P.speed,p=globe(P,i,yaw),s=.5+.5*Math.sin(2*phi+lam-TAU*t/.9),lit=cl(s*1.4-.4);
   return Q(p,lit*.75,(.9+.25*lit)*P.dot,.5+.5*lit)}},
 waiting:{tone:"warning",tempo:1.8,gen(i,t,P){return breath(P,i,.5+.5*Math.sin(TAU*t/1.8))}}, // the globe stops and breathes, asking for you
 done:{tone:"success",tempo:0,plays_once:true,gen(i,t,P){ // the globe fills up green over .5s, pops once, then holds
   const d=breath(P,i,cl(t/.5)),pop=t>.5&&t<.8?Math.sin(Math.PI*(t-.5)/.3):0;return Q(d.p,d.lit,d.s*(1+.15*pop),d.a)}},
 error:{tone:"danger",tempo:0,plays_once:true,gen(i,t,P){ // the full red globe shakes once, like a head saying no, then holds
   const st=t-.3,dx=st>0&&st<.9?.13*Math.exp(-5*st)*Math.sin(28*st):0,d=breath(P,i,1);return Q([d.p[0]+dx,d.p[1],d.p[2]],d.lit,d.s,d.a)}},
 offline:{tone:"none",dim:true,tempo:2.4,gen(i,t,P){ // the globe dims and its rings drop out, one flicker at a time
   const [phi,lam,j]=P.grid[i],p=globe(P,i,.12*t,R*.92),slot=Math.floor(t/.4),off=hash2(j,slot)<.42||hash2(i,slot)<.12;
   return Q(p,0,(off?0:.85)*P.dot,off?0:.45)}},
};
const ORDER=["thinking","reading","searching","editing","running","waiting","done","error","offline"];
const STILL={thinking:.55,reading:.6,searching:.3,editing:1.1,running:.3,waiting:.45,done:1.5,error:1.5,offline:.3};
const MORPH=.6,STAG_TOTAL=.3; // dot i starts hash(i)·STAG_TOTAL late: a dissolve across the globe, whatever its size
const SETTLE=1.3; // Done and Error are settled this long after they start
const crit=u=>u<=0?0:u>=1.6?1:1-(1+7*u)*Math.exp(-7*u);
// Done or Error right after Waiting: no morph; the breath keeps going and fills up to the full bright globe over SETTLE.
const fromWaiting=(tl,k)=>k>0&&ORBS[tl[k].orb].plays_once&&tl[k-1].orb==="waiting";
// Segment k's own dot i at T (what it generates, before any morph).
function genAt(tl,k,i,T,P){const seg=tl[k],tin=T-seg.start;
  if(fromWaiting(tl,k)){const e=crit(1.6*tin/SETTLE),b=.5+.5*Math.sin(TAU*(T-tl[k-1].start)/1.8);return breath(P,i,b+(1-b)*e)}
  return ORBS[seg.orb].gen(i,tin,P)}
// tl: [{orb, start}], consecutive entries with different orbs. P: LARGE or SMALL.
function sample(tl,T,P=LARGE){let k=0;while(k+1<tl.length&&tl[k+1].start<=T)k++;const cur=tl[k],tin=T-cur.start,S=ORBS[cur.orb];
  const out=[];for(let i=0;i<P.N;i++){const b=genAt(tl,k,i,T,P);
    if(k===0){out.push({...b,tone:S.tone,dim:S.dim});continue}
    if(fromWaiting(tl,k)){out.push({...b,lit:cl(b.lit+(tin<.15?.8*Math.sin(Math.PI*tin/.15):0)),tone:S.tone,dim:S.dim});continue}
    const pv=tl[k-1],Pv=ORBS[pv.orb],a=genAt(tl,k-1,i,T,P),q=crit((tin-hash(i)*STAG_TOTAL)/MORPH),lift=.55*Math.sin(Math.PI*q);
    out.push({p:[a.p[0]+(b.p[0]-a.p[0])*q,a.p[1]+(b.p[1]-a.p[1])*q,a.p[2]+(b.p[2]-a.p[2])*q+lift],lit:cl(a.lit+(b.lit-a.lit)*q+(tin<.15?.8*Math.sin(Math.PI*tin/.15):0)),s:a.s+(b.s-a.s)*q,
      a:a.a+(b.a-a.a)*q,tone:q>.5?S.tone:Pv.tone,dim:q>.5?S.dim:Pv.dim})}return out}
const FINE=.6,TINT=.48,GLOW=2; // dots drawn at 60% (coloured ones 48%) so the globe reads through the glow
function css(v){return getComputedStyle(document.body).getPropertyValue(v).trim()}
function drawOrb(canvas,tl,T){const dpr=(window.devicePixelRatio||1),size=canvas.clientWidth;canvas.width=Math.round(size*dpr);canvas.height=Math.round(size*dpr);
  const ctx=canvas.getContext('2d');ctx.scale(dpr,dpr);ctx.clearRect(0,0,size,size);
  const small=size<=30,P=small?SMALL:LARGE,Rpx=size*.4,base=small?size*.045:size*.011,px=1/dpr;
  const col={none:css('--orb-dot'),warning:css('--warning'),success:css('--success'),danger:css('--danger')};
  const pts=sample(tl,T,P),tone=pts[0]?.tone||'none';
  canvas.style.filter=tone==='none'?'':`drop-shadow(0 0 ${GLOW}px ${col[tone]})`; // coloured dots glow, and keep glowing once settled
  const pr=pts.map(b=>{const z=b.p[2],k=F/(F-z),dn=cl((z+1.2)/2.4);return {...b,x:size/2+b.p[0]*k*Rpx,y:size/2-b.p[1]*k*Rpx,z,k,dn}}).sort((a,b)=>a.z-b.z);
  for(const b of pr){if(b.s<=.02||b.k<=.02)continue;const ln=Math.hypot(...b.p)||1,lam=(-.55*b.p[0]+.6*b.p[1]+.58*b.p[2])/ln*.5+.5;let r=base*b.s*b.k*(0.4+0.6*b.dn)*(.9+.2*lam)*(1+.55*b.lit)*(b.tone&&b.tone!=='none'?TINT:FINE);r=Math.max(px*.75,r);
    let a=(.12+.88*b.dn)*(.62+.48*lam);a=cl(a+.5*b.lit)*b.a;if(b.dim)a*=.85;ctx.globalAlpha=cl(a);ctx.fillStyle=col[b.tone||'none'];
    if(small&&r*dpr<1.6){ctx.fillRect(Math.round((b.x-r)*dpr)/dpr,Math.round((b.y-r)*dpr)/dpr,Math.max(px,Math.round(2*r*dpr)/dpr),Math.max(px,Math.round(2*r*dpr)/dpr))}
    else{ctx.beginPath();ctx.arc(b.x,b.y,r,0,TAU);ctx.fill()}}
  ctx.globalAlpha=1}
if(typeof module!=="undefined")module.exports={LARGE,SMALL,ORBS,ORDER,STILL,sample,hash,vnoise,breath};
