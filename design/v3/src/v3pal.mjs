// v3 neutral palette. `accent` is the single brand slot (default: macOS system blue; the app can use CSS AccentColor).
// sidebarApprox = the vibrancy sidebar composited over a typical wallpaper, for contrast checks.
export const V3 = {
  dark: { bg:"#161618", panel:"#1C1C1F", raised:"#232327", sidebarApprox:"#262629", line:"#2C2C31", text:"#F2F2F3", muted:"#A0A0A7", faint:"#8C8C93",
    control:"#6A6A72", accent:"#1F6FEB", onAccent:"#FFFFFF", accentText:"#6AA8FF", accentSoft:"#17294A", good:"#4CC38A", bad:"#FF7B72", focus:"#4C9AFF" },
  light: { bg:"#FFFFFF", panel:"#F7F7F8", raised:"#F0F0F2", sidebarApprox:"#E8E8EC", line:"#E4E4E8", text:"#1D1D1F", muted:"#5F5F66", faint:"#6B6B72",
    control:"#8A8A92", accent:"#0A64E8", onAccent:"#FFFFFF", accentText:"#0A5CD6", accentSoft:"#E6F0FF", good:"#1A7F4B", bad:"#C4281C", focus:"#0A64E8" },
};
const lum=h=>{const c=h.slice(1).match(/../g).map(x=>parseInt(x,16)/255).map(v=>v<=.03928?v/12.92:((v+.055)/1.055)**2.4);return .2126*c[0]+.7152*c[1]+.0722*c[2]};
export const ratio=(a,b)=>{const[x,y]=[lum(a),lum(b)].sort((p,q)=>q-p);return(x+.05)/(y+.05)};
const P=[["text","bg",4.5],["text","sidebarApprox",4.5],["muted","bg",4.5],["muted","panel",4.5],["muted","raised",4.5],["muted","sidebarApprox",4.5],["faint","bg",4.5],["faint","panel",4.5],
["accentText","bg",4.5],["accentText","panel",4.5],["accentText","accentSoft",4.5],["text","accentSoft",4.5],["onAccent","accent",4.5],["good","bg",4.5],["bad","bg",4.5],["control","bg",3],["control","panel",3],["focus","bg",3],["focus","panel",3]];
if(process.argv[1].endsWith("v3pal.mjs")){let f=0;const mins={};for(const[t,p]of Object.entries(V3))for(const[a,b,m]of P){const r=ratio(p[a],p[b]);mins[a+"/"+b]=Math.min(mins[a+"/"+b]??99,r);if(r<m){f++;console.log("FAIL",t,a,b,r.toFixed(2))}}
console.log(Object.entries(mins).map(([k,v])=>k+" "+v.toFixed(1)).join(", "));console.log(f?f+" FAIL":"all AA")}
