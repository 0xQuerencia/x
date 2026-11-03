(function(){
var runs={
 dictate:{task:"hello. press the key, talk, release.",phase:"ready",run:0,rows:[["state","listening"],["state","dictating"],["state","transcribing"],["typed","Hello."]],note:"no wake word, so it is just typed where your cursor is."},
 actions:{task:"Hey Friday, open firefox",phase:"completed",run:0,rows:[["info","Trusted Actions: running without a prompt"],["info","Approved"],["action","Open firefox"],["result","Launched (outcome not verified)"],["info","Task completed"]],note:"simple launches skip the prompt once you turn trust on. it says plainly that a launch is not a check."},
 work:{task:"fix the failing tests (work)",phase:"completed",run:0,rows:[["info","Approved"],["action","Run OpenCode in your project folder"],["permission","Blocked bash: a command outside the policy"],["info","Allowing it for this rerun only"],["result","changed files: hello.txt"]],note:"opencode stays inside the folder you chose. blocked commands wait for you."},
 computer:{task:"go to example dot org in chromium",phase:"completed",run:0,rows:[["action","Operate chromium: go to example.org"],["observe","\"X test page - Chromium\": 17 controls"],["act","go to https://example.org"],["verify","The screen changed after the last step"],["observe","\"Example Domain - Chromium\": 16 controls"],["verify","Found on screen: \"Example Domain\""],["info","Task completed"]],note:"it only says done after it can quote text that is on screen."}
};
var $=function(s,r){return (r||document).querySelector(s)},$$=function(s,r){return [].slice.call((r||document).querySelectorAll(s))};
var timer;
function show(k){
  var r=runs[k],log=$("#demo-log");clearTimeout(timer);
  $("#demo-task").textContent=r.task;$("#demo-note").textContent="";log.innerHTML="";
  $("#demo-phase").textContent="running";$("#demo-dot").className="dot run";
  var i=0;(function next(){
    if(i>=r.rows.length){$("#demo-phase").textContent=r.phase;$("#demo-dot").className="dot";$("#demo-note").textContent=r.note;return}
    var li=document.createElement("li");li.className=r.rows[i][0];
    li.innerHTML='<span class="k"></span><span class="t"></span>';
    li.firstChild.textContent=r.rows[i][0];li.lastChild.textContent=r.rows[i][1];log.appendChild(li);i++;
    timer=setTimeout(next,matchMedia("(prefers-reduced-motion:reduce)").matches?0:520);
  })();
}
$$("#demo-tabs button").forEach(function(b){b.onclick=function(){$$("#demo-tabs button").forEach(function(x){x.classList.remove("on")});b.classList.add("on");show(b.dataset.k)}});
show("dictate");

$$(".codetabs .tabs button").forEach(function(b){b.onclick=function(){
  $$(".codetabs .tabs button").forEach(function(x){x.classList.toggle("on",x===b)});
  $$(".codetabs .code").forEach(function(c){c.hidden=c.dataset.c!==b.dataset.c})}});
$$(".copy").forEach(function(b){b.onclick=function(){
  var t=$("pre",b.parentNode).textContent;
  (navigator.clipboard?navigator.clipboard.writeText(t):Promise.reject()).then(function(){b.textContent="copied"},function(){b.textContent="press ctrl+c"});
  setTimeout(function(){b.textContent="copy"},1400)}});

var slides=[
 ["see what it is doing","every step lands in the activity list, with what it observed and what it did.","computer"],
 ["it checks its own work","a step that changed nothing stops the task instead of looping.","computer"],
 ["simple things stay simple","opening an app is one line and no prompt, once you trust it.","actions"],
 ["coding work goes to opencode","a headless run in a throwaway folder, with the files it changed listed after.","work"],
 ["plain speech stays plain","say anything without the wake word and it is just typed.","dictate"]
];
var host=$(".slides");
slides.forEach(function(s,i){
  var d=document.createElement("div");d.className="slide"+(i?"":" on");
  var r=runs[s[2]],rows=r.rows.slice(0,5).map(function(x){return '<li style="opacity:1;animation:none" class="'+x[0]+'"><span class="k">'+x[0]+'</span><span class="t"></span></li>'}).join("");
  d.innerHTML='<div class="shot"><div class="win"><div class="bar"><span class="dot"></span><b>'+r.phase+'</b><span class="spacer"></span><span class="mini">agent</span></div><p class="task"></p><ul class="log" style="min-height:0">'+rows+'</ul></div></div><h3></h3><p class="d"></p>';
  $(".task",d).textContent=r.task;$$(".t",d).forEach(function(t,j){t.textContent=r.rows[j][1]});
  $("h3",d).textContent=s[0];$(".d",d).textContent=s[1];host.appendChild(d)});
var cur=0;function go(n){cur=(n+slides.length)%slides.length;$$(".slide").forEach(function(s,i){s.classList.toggle("on",i===cur)});$("#count").textContent=(cur+1)+" / "+slides.length}
$("#prev").onclick=function(){go(cur-1)};$("#next").onclick=function(){go(cur+1)};

var secs=$$("main section[id],header[id]"),links=$$(".pill a[href^='#']");
var io=new IntersectionObserver(function(es){es.forEach(function(e){if(e.isIntersecting){
  var id=e.target.id==="install"?"what":e.target.id;
  links.forEach(function(a){a.classList.toggle("on",a.getAttribute("href")==="#"+id)})}})},{rootMargin:"-40% 0px -55% 0px"});
secs.forEach(function(s){io.observe(s)});

$("#theme").onclick=function(){
  var d=document.documentElement,n=d.dataset.theme==="dark"?"light":"dark";d.dataset.theme=n;
  try{localStorage.setItem("x-theme",n)}catch(e){}
  $('meta[name="theme-color"]').content=n==="dark"?"#1b1b1b":"#f1f1f1"};
})();
