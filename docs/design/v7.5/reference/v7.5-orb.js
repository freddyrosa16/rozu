// rozu v7.5 activity orbs: a dotted globe for every state.
//
// Every state is the same object, a sphere of small matte dots laid out on lines of latitude, and the state
// is what the globe is doing: thought weather drifting over it (thinking), lines of text being written round
// it (notes), a reading band descending it (reading), a lens roaming its surface (searching), a caret retyping
// its equator (editing), a fast helical spin (running), a slow breath (waiting), a flicker of dropped rings
// (offline). Done and Error gather the dots into the check and the cross from v7.4, with the rest of the dots
// resting as a faint ring. Same perspective, light, depth and morph as v7.3/v7.4.
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
const globe=(P,i,yaw,r=R)=>{const d=dir(P,i);return rX(rY([d[0]*r,d[1]*r,d[2]*r],yaw),TILT)};
// Where dot i faces the camera: 0 at the front, ±π at the back (after a spin of `yaw`).
const front=(P,i,yaw)=>{const a=P.grid[i][1]-yaw-Math.PI/2;return Math.atan2(Math.sin(a),Math.cos(a))};
// Point at arc-length fraction u along a set of polylines, as one path.
const along=(strokes,u)=>{const segs=[];let L=0;for(const pts of strokes)for(let k=0;k+1<pts.length;k++){const l=Math.hypot(pts[k+1][0]-pts[k][0],pts[k+1][1]-pts[k][1]);segs.push([pts[k],pts[k+1],l]);L+=l}
  let d=u*L;for(let k=0;k<segs.length;k++){const [a,b,l]=segs[k];if(d<=l||k===segs.length-1){const f=l?cl(d/l):0;return [a[0]+(b[0]-a[0])*f,a[1]+(b[1]-a[1])*f]}d-=l}};
const CHECK=[[-.46,.02],[-.14,-.32],[.48,.34]];
const CROSS=[[[-.36,.36],[.36,-.36]],[[.36,.36],[-.36,-.36]]];
const GZ=.3;
// Done and Error: the first `share` of the dots draw the symbol, the rest rest on a faint dotted ring.
function symbol(P,i,strokes,share,dx,drawn,pop,litOff){const n=Math.round(P.N*share);
  if(i<n){const u=i/(n-1),xy=along(strokes,u),on=u<=drawn;return Q([xy[0]+dx,xy[1],GZ],on?1:litOff,(on?1.2:.6)*(1+.2*pop)*P.dot,on?1:.35)}
  const a=Math.PI/2-TAU*(i-n)/(P.N-n);return Q([R*Math.cos(a)+dx,R*Math.sin(a),GZ],0,.7*P.dot,.3)}
const ORBS={
 thinking:{tone:"none",tempo:1.6,gen(i,t,P){ // thought weather: patches of light drift over a slowly turning globe
   const [phi,lam]=P.grid[i],p=globe(P,i,.5*t*P.speed),n=vnoise(lam*1.4+.4*t,phi*1.3+.2*t+7+lam*.3),lit=cl(n*2.2-.9);
   return Q(p,lit*.7,(.9+.3*lit)*P.dot,.5+.5*lit)}},
 notes:{tone:"none",tempo:2.6,gen(i,t,P){ // three lines of text write themselves round the globe, hold, then start over
   const [phi,lam,j]=P.grid[i],p=globe(P,i,0),mid=(P.rings-1)/2,lines=[mid-2,mid,mid+2].map(Math.round),li=lines.indexOf(j);
   if(li<0)return Q(p,0,.75*P.dot,.3);
   const ph=(t%2.6)/1.9,w=cl(ph*3-li),u=((lam+Math.PI/2)%TAU)/TAU,on=u<=w,head=on&&w<1&&u>w-.06;
   return Q(p,on?1:0,(head?1.6:on?1.15:.75)*P.dot,on?1:.3)}},
 reading:{tone:"none",tempo:2.2,gen(i,t,P){ // a lit band reads down the globe, line by line
   const [phi]=P.grid[i],p=globe(P,i,.25*t*P.speed),yb=Math.PI/2-Math.PI*((t%2.2)/2.2)*1.15+.07,on=Math.exp(-Math.pow((phi-yb)/.17,2));
   return Q(p,on,(.8+.5*on)*P.dot,.35+.65*on)}},
 searching:{tone:"none",tempo:2.4,gen(i,t,P){ // a lens roams the surface; the dots under it swell
   const [phi,lam]=P.grid[i],yaw=.3*t*P.speed,p=globe(P,i,yaw),a=TAU*t/2.4,cphi=.35*Math.sin(a*.7),clam=front(P,i,yaw)-(.9*Math.sin(a));
   const d=Math.hypot(phi-cphi,clam*Math.cos(cphi)),inside=cl((.42-d)/.12),lit=inside;
   return Q(p,lit,(.85+.55*lit)*P.dot,.4+.6*lit)}},
 editing:{tone:"none",tempo:2.4,gen(i,t,P){ // a caret moves along the equator, retyping the dots behind it
   const [phi,lam,j]=P.grid[i],p=globe(P,i,0),eq=Math.round((P.rings-1)/2);
   if(j!==eq)return Q(p,0,.8*P.dot,.4);
   const ph=(t%2.4),w=cl(ph/1.6),cx=-Math.PI*.78+1.56*Math.PI*w,f=front(P,i,0),blink=w>=1&&Math.floor((ph-1.6)/.2)%2===1;
   const caret=Math.abs(f-cx)<Math.PI/P.lon;if(caret)return Q(p,blink?0:1,(blink?.6:1.7)*P.dot,blink?.3:1);
   const behind=cx-f,fresh=behind>0&&behind<1.2?1-behind/1.2:0;return Q(p,f<cx?.5+.5*fresh*fresh:0,(1+.4*fresh)*P.dot,f<cx?1:.35)}},
 running:{tone:"none",tempo:.9,gen(i,t,P){ // the globe spins fast with a helical stripe racing round it
   const [phi,lam]=P.grid[i],yaw=2.4*t*P.speed,p=globe(P,i,yaw),s=.5+.5*Math.sin(2*phi+lam-TAU*t/.9),lit=cl(s*1.4-.4);
   return Q(p,lit*.75,(.9+.25*lit)*P.dot,.5+.5*lit)}},
 waiting:{tone:"warning",tempo:1.8,gen(i,t,P){ // the globe stops and breathes, asking for you
   const b=.5+.5*Math.sin(TAU*t/1.8),p=globe(P,i,0,R*(.96+.06*b));return Q(p,.2+.5*b,(.9+.25*b)*P.dot,.6+.4*b)}},
 done:{tone:"success",tempo:0,plays_once:true,gen(i,t,P){ // the dots gather into a check that draws itself, pops, then holds
   return symbol(P,i,[CHECK],.42,0,cl((t-.25)/.45),t>.7&&t<.95?Math.sin(Math.PI*(t-.7)/.25):0,.25)}},
 error:{tone:"danger",tempo:0,plays_once:true,gen(i,t,P){ // a cross that shakes once, like a head saying no, then holds
   const st=t-.3,dx=st>0&&st<.9?.13*Math.exp(-5*st)*Math.sin(28*st):0;return symbol(P,i,CROSS,.42,dx,1,0,1)}},
 offline:{tone:"none",dim:true,tempo:2.4,gen(i,t,P){ // the globe dims and its rings drop out, one flicker at a time
   const [phi,lam,j]=P.grid[i],p=globe(P,i,.12*t,R*.92),slot=Math.floor(t/.4),off=hash2(j,slot)<.42||hash2(i,slot)<.12;
   return Q(p,0,(off?0:.85)*P.dot,off?0:.45)}},
};
const ORDER=["thinking","notes","reading","searching","editing","running","waiting","done","error","offline"];
const STILL={thinking:.55,notes:1.2,reading:.6,searching:.3,editing:1.1,running:.3,waiting:.45,done:1.5,error:1.5,offline:.3};
const MORPH=.6,STAG_TOTAL=.3; // dot i starts hash(i)·STAG_TOTAL late: a dissolve across the globe, whatever its size
const crit=u=>u<=0?0:u>=1.6?1:1-(1+7*u)*Math.exp(-7*u);
// tl: [{orb, start}], consecutive entries with different orbs. P: LARGE or SMALL.
function sample(tl,T,P=LARGE){let k=0;while(k+1<tl.length&&tl[k+1].start<=T)k++;const cur=tl[k],tin=T-cur.start,S=ORBS[cur.orb];
  const out=[];for(let i=0;i<P.N;i++){const b=S.gen(i,tin,P);
    if(k===0){out.push({...b,tone:S.tone,dim:S.dim});continue}
    const pv=tl[k-1],Pv=ORBS[pv.orb],a=Pv.gen(i,T-pv.start,P),q=crit((tin-hash(i)*STAG_TOTAL)/MORPH),lift=.55*Math.sin(Math.PI*q);
    out.push({p:[a.p[0]+(b.p[0]-a.p[0])*q,a.p[1]+(b.p[1]-a.p[1])*q,a.p[2]+(b.p[2]-a.p[2])*q+lift],lit:cl(a.lit+(b.lit-a.lit)*q+(tin<.15?.8*Math.sin(Math.PI*tin/.15):0)),s:a.s+(b.s-a.s)*q,
      a:a.a+(b.a-a.a)*q,tone:q>.5?S.tone:Pv.tone,dim:q>.5?S.dim:Pv.dim})}return out}
function css(v){return getComputedStyle(document.body).getPropertyValue(v).trim()}
function drawOrb(canvas,tl,T){const dpr=(window.devicePixelRatio||1),size=canvas.clientWidth;canvas.width=Math.round(size*dpr);canvas.height=Math.round(size*dpr);
  const ctx=canvas.getContext('2d');ctx.scale(dpr,dpr);ctx.clearRect(0,0,size,size);
  const small=size<=30,P=small?SMALL:LARGE,Rpx=size*.4,base=small?size*.045:size*.011,px=1/dpr;
  const col={none:css('--orb-dot'),warning:css('--warning'),success:css('--success'),danger:css('--danger')};
  const pts=sample(tl,T,P);
  const pr=pts.map(b=>{const z=b.p[2],k=F/(F-z),dn=cl((z+1.2)/2.4);return {...b,x:size/2+b.p[0]*k*Rpx,y:size/2-b.p[1]*k*Rpx,z,k,dn}}).sort((a,b)=>a.z-b.z);
  for(const b of pr){if(b.s<=.02||b.k<=.02)continue;const ln=Math.hypot(...b.p)||1,lam=(-.55*b.p[0]+.6*b.p[1]+.58*b.p[2])/ln*.5+.5;let r=base*b.s*b.k*(0.4+0.6*b.dn)*(.9+.2*lam)*(1+.55*b.lit);r=Math.max(px*.75,r);
    let a=(.12+.88*b.dn)*(.62+.48*lam);a=cl(a+.5*b.lit)*b.a;if(b.dim)a*=.85;ctx.globalAlpha=cl(a);ctx.fillStyle=col[b.tone||'none'];
    if(small&&r*dpr<1.6){ctx.fillRect(Math.round((b.x-r)*dpr)/dpr,Math.round((b.y-r)*dpr)/dpr,Math.max(px,Math.round(2*r*dpr)/dpr),Math.max(px,Math.round(2*r*dpr)/dpr))}
    else{ctx.beginPath();ctx.arc(b.x,b.y,r,0,TAU);ctx.fill()}}
  ctx.globalAlpha=1}
if(typeof module!=="undefined")module.exports={LARGE,SMALL,ORBS,ORDER,STILL,sample,hash,vnoise};
