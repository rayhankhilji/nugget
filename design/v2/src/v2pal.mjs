export const V2 = {
  dark: { bg:"#0F0F11", panel:"#151518", raised:"#1E1E22", line:"#27272C", text:"#EDEDEF", muted:"#9B9BA4", control:"#64646D",
    gold:"#FFC53D", onGold:"#1A1406", goldText:"#FFD36B", goldSoft:"#2C2410", focus:"#FFD36B", good:"#5BD39A", bad:"#FF8A80", scrim:"rgb(5 5 7 / .62)" },
  light: { bg:"#FFFFFF", panel:"#F7F7F5", raised:"#EEEEEB", line:"#E6E6E2", text:"#121214", muted:"#5E5E66", control:"#8A8A92",
    gold:"#FFC53D", onGold:"#1A1406", goldText:"#875B00", goldSoft:"#FFF3D1", focus:"#875B00", good:"#1D7A4C", bad:"#B3261E", scrim:"rgb(18 18 20 / .32)" },
};
const lum=h=>{const c=h.slice(1).match(/../g).map(x=>parseInt(x,16)/255).map(v=>v<=.03928?v/12.92:((v+.055)/1.055)**2.4);return .2126*c[0]+.7152*c[1]+.0722*c[2]};
export const ratio=(a,b)=>{const[x,y]=[lum(a),lum(b)].sort((p,q)=>q-p);return(x+.05)/(y+.05)};
const P=[["text","bg",4.5],["text","panel",4.5],["text","raised",4.5],["muted","bg",4.5],["muted","panel",4.5],["muted","raised",4.5],["goldText","bg",4.5],["goldText","panel",4.5],["text","goldSoft",4.5],["goldText","goldSoft",4.5],["onGold","gold",4.5],["good","bg",4.5],["bad","bg",4.5],["control","bg",3],["control","panel",3],["focus","bg",3],["focus","panel",3]];
if(process.argv[1].endsWith("v2pal.mjs")){let f=0;for(const[t,p]of Object.entries(V2))for(const[a,b,m]of P){const r=ratio(p[a],p[b]);if(r<m){f++;console.log("FAIL",t,a,b,r.toFixed(2))}}
const mins={};for(const[t,p]of Object.entries(V2))for(const[a,b]of P){const k=a+"/"+b,r=ratio(p[a],p[b]);mins[k]=Math.min(mins[k]??99,r)}
console.log(Object.entries(mins).map(([k,v])=>k+" "+v.toFixed(1)).join(", "));console.log(f?f+" FAIL":"all AA")}
