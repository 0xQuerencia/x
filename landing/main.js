(function(){
var $=function(s,r){return (r||document).querySelector(s)},$$=function(s,r){return [].slice.call((r||document).querySelectorAll(s))};
$$(".codetabs .tabs button").forEach(function(b){b.onclick=function(){
  $$(".codetabs .tabs button").forEach(function(x){x.classList.toggle("on",x===b)});
  $$(".codetabs .code").forEach(function(c){c.hidden=c.dataset.c!==b.dataset.c})}});
$$(".copy").forEach(function(b){b.onclick=function(){
  var t=$("pre",b.parentNode).textContent;
  (navigator.clipboard?navigator.clipboard.writeText(t):Promise.reject()).then(function(){b.textContent="copied"},function(){b.textContent="press ctrl+c"});
  setTimeout(function(){b.textContent="copy"},1400)}});

var slides=[
 ["models","choose a speech model","the picker shows the wait, wrong-word rate and languages for each model before you download it."],
 ["dictation","the settings that matter","your shortcut, how it triggers, the model, a vocabulary list and the microphone, in one place."],
 ["agent","the optional agent pane","controls on top, the plan below, then the activity list. this is the part that acts, and it is off until you turn it on."],
 ["controls","choose how it listens","pick what a spoken sentence does, switch trusted actions on, and see which browsers x may read. every grant has a revoke button."],
 ["task","it says what it did","a finished browser task, with the step it took and what it saw on screen to call it done. this run used a scripted stand-in model on a test page."],
 ["result","and the page it landed on","the link was clicked by x through the accessibility tree, on a plain local test page."]
];
var shots={
  dictation:{l:38.33,t:5.13,w:63.33,h:75.56,alt:"the x settings window: shortcut, speech model, vocabulary and microphone"},
  models:{l:16.67,t:1.28,w:66.67,h:79.54,alt:"the speech model picker comparing wait, wrong words and languages"},
  agent:{l:39.17,t:7.69,w:68.33,h:81.5,alt:"the x agent pane after a finished browser task"},
  result:{l:18.33,t:26,w:61.67,h:29.4,alt:"a browser page x opened by clicking a link"}
};
Object.keys(shots).forEach(function(n){var i=new Image();i.src="img/win-"+n+".webp"});
var morph=$("#morph"),layers=[$("#layer-a"),$("#layer-b")],front=0,morphToken=0;
var canVT=!!document.startViewTransition;
if(canVT)document.documentElement.classList.add("vt");
function morphTo(name){
  var s=shots[name],token=++morphToken,reduce=matchMedia("(prefers-reduced-motion:reduce)").matches;
  var loaded=new Image();loaded.src="img/win-"+name+".webp";
  (loaded.decode?loaded.decode():Promise.resolve()).catch(function(){}).then(function(){
    if(token!==morphToken)return;
    var apply=function(){
      var next=layers[1-front],cur=layers[front];
      next.src=loaded.src;
      morph.style.left=s.l+"%";morph.style.top=s.t+"%";morph.style.width=s.w+"%";morph.style.height=s.h+"%";
      next.classList.add("on");cur.classList.remove("on");front=1-front;
      $("#field").setAttribute("aria-label",s.alt)};
    if(canVT&&!reduce)document.startViewTransition(apply);else apply()})}
$$("#hero-tabs button").forEach(function(b){b.onclick=function(){
  if(b.classList.contains("on"))return;
  $$("#hero-tabs button").forEach(function(x){x.classList.toggle("on",x===b)});
  morphTo(b.dataset.i)}});
var host=$(".slides");
slides.forEach(function(s,i){
  var d=document.createElement("div");d.className="slide"+(i?"":" on");
  d.innerHTML='<picture><source srcset="img/'+s[0]+'.webp" type="image/webp"><img src="img/'+s[0]+'.png" width="1200" height="780" loading="lazy" alt=""></picture><h3></h3><p class="d"></p>';
  $("img",d).alt=s[1];$("h3",d).textContent=s[1];$(".d",d).textContent=s[2];host.appendChild(d)});
var cur=0;function go(n){cur=(n+slides.length)%slides.length;$$(".slide").forEach(function(s,i){s.classList.toggle("on",i===cur)});$("#count").textContent=(cur+1)+" / "+slides.length}
$("#prev").onclick=function(){go(cur-1)};$("#next").onclick=function(){go(cur+1)};

var secs=$$("main section[id],header[id]"),links=$$(".pill a[href^='#']");
var io=new IntersectionObserver(function(es){es.forEach(function(e){if(e.isIntersecting){
  var id=e.target.id==="install"?"dictation":e.target.id;
  links.forEach(function(a){a.classList.toggle("on",a.getAttribute("href")==="#"+id)})}})},{rootMargin:"-40% 0px -55% 0px"});
secs.forEach(function(s){io.observe(s)});

$("#theme").onclick=function(){
  var d=document.documentElement,n=d.dataset.theme==="dark"?"light":"dark";d.dataset.theme=n;
  try{localStorage.setItem("x-theme",n)}catch(e){}
  $('meta[name="theme-color"]').content=n==="dark"?"#1b1b1b":"#f1f1f1"};
})();
