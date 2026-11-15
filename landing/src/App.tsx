
import { flushSync } from "react-dom";
import { useEffect, useRef, useState } from "react";

type Shot = "dictation" | "models" | "agent" | "result";
const SHOTS: Record<Shot, { l: string; t: string; w: string; h: string; alt: string }> = {
  dictation: { l: "38.33%", t: "5.13%", w: "63.33%", h: "75.56%", alt: "the x settings window: shortcut, speech model, vocabulary and microphone" },
  models: { l: "16.67%", t: "1.28%", w: "66.67%", h: "79.54%", alt: "the speech model picker comparing wait, wrong words and languages" },
  agent: { l: "39.17%", t: "7.69%", w: "68.33%", h: "81.5%", alt: "the x agent pane after a finished browser task" },
  result: { l: "18.33%", t: "26%", w: "61.67%", h: "29.4%", alt: "a browser page x opened by clicking a link" },
};
const HERO_TABS: { key: Shot; label: string }[] = [
  { key: "dictation", label: "dictation" },
  { key: "models", label: "speech models" },
  { key: "agent", label: "agent" },
  { key: "result", label: "browser" },
];
const SLIDES = [
  ["models", "choose a speech model", "the picker shows the wait, wrong-word rate and languages for each model before you download it."],
  ["dictation", "the settings that matter", "your shortcut, how it triggers, the model, a vocabulary list and the microphone, in one place."],
  ["agent", "the optional agent pane", "controls on top, the plan below, then the activity list. this is the part that acts, and it is off until you turn it on."],
  ["controls", "choose how it listens", "pick what a spoken sentence does, switch trusted actions on, and see which browsers x may read. every grant has a revoke button."],
  ["task", "it says what it did", "a finished browser task, with the step it took and what it saw on screen to call it done. this run used a scripted stand-in model on a test page."],
  ["result", "and the page it landed on", "the link was clicked by x through the accessibility tree, on a plain local test page."],
] as const;
const COMMANDS = [
  ["script", "git clone https://github.com/0xQuerencia/x && cd x\n./scripts/install-arch.sh\nsystemctl --user enable --now x.service\nx labs on x-agent   # only if you want the agent"],
  ["aur", "# package files are in packaging/aur-x\ncd packaging/aur-x && makepkg -si"],
  ["check", "sha256sum x-0.1.0-alpha.1-linux-x64\n# compare with the .sha256 file on the release"],
] as const;

function SiteNav({ active }: { active: string }) {
  const links = [
    ["#top", "x"], ["#dictation", "dictation"], ["#agent", "agent"],
    ["#look", "screens"], ["#faq", "faqs"],
    ["https://github.com/0xQuerencia/x/releases", "changelog"],
    ["https://github.com/0xQuerencia/x", "github"],
  ];
  return <nav className="pill" aria-label="Sections">
    {links.map(([href, label]) => <a key={href} href={href} className={href === "#" + active ? "on" : undefined}>{label}</a>)}
  </nav>;
}

function HeroShow() {
  const [tab, setTab] = useState<Shot>("dictation");
  const [stage, setStage] = useState<{ key: Shot; front: 0 | 1; images: [Shot, Shot] }>({
    key: "dictation", front: 0, images: ["dictation", "dictation"],
  });
  const request = useRef(0);
  useEffect(() => {
    for (const key of Object.keys(SHOTS) as Shot[]) { const img = new Image(); img.src = "img/win-" + key + ".webp"; }
    if ("startViewTransition" in document) document.documentElement.classList.add("vt");
    return () => { ++request.current; document.documentElement.classList.remove("vt"); };
  }, []);
  async function choose(key: Shot) {
    if (tab === key) return;
    setTab(key);
    const token = ++request.current;
    const img = new Image();
    img.src = "img/win-" + key + ".webp";
    try { await img.decode(); } catch { /* keep previous image when loading fails */ }
    if (request.current !== token) return;
    const update = () => flushSync(() => setStage(prev => {
      const images: [Shot, Shot] = [...prev.images];
      const front: 0 | 1 = prev.front === 0 ? 1 : 0;
      images[front] = key;
      return { key, front, images };
    }));
    const doc = document as Document & { startViewTransition?: (callback: () => void) => unknown };
    if (doc.startViewTransition && !matchMedia("(prefers-reduced-motion:reduce)").matches) doc.startViewTransition(update);
    else update();
  }
  const s = SHOTS[stage.key];
  return <>
    <div className="tabs" role="tablist" id="hero-tabs">
      {HERO_TABS.map(({ key, label }) => <button key={key} type="button" role="tab" className={tab === key ? "on" : undefined}
        data-i={key} data-a={SHOTS[key].alt} aria-selected={tab === key} onClick={() => void choose(key)}>{label}</button>)}
    </div>
    <div className="stage" id="field" role="img" aria-label={s.alt}>
      <div className="win" id="morph" style={{ left: s.l, top: s.t, width: s.w, height: s.h }}>
        {stage.images.map((key, i) => <img key={i} id={i ? "layer-b" : "layer-a"}
          className={"layer" + (stage.front === i ? " on" : "")} src={"img/win-" + key + ".webp"} alt="" />)}
      </div>
    </div>
  </>;
}

function InstallTabs() {
  const [tab, setTab] = useState<(typeof COMMANDS)[number][0]>("script");
  const [status, setStatus] = useState("");
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  async function copy(code: string) {
    clearTimeout(timer.current);
    try { await navigator.clipboard.writeText(code); setStatus("copied"); }
    catch { setStatus("press ctrl+c"); }
    timer.current = setTimeout(() => setStatus(""), 1400);
  }
  return <div className="codetabs">
    <div className="tabs small" role="tablist">
      {COMMANDS.map(([key]) => <button key={key} type="button" role="tab" data-c={key} aria-selected={tab === key}
        className={tab === key ? "on" : undefined} onClick={() => { setTab(key); setStatus(""); }}>{key === "check" ? "verify" : key}</button>)}
    </div>
    {COMMANDS.map(([key, code]) => <div key={key} className="code" data-c={key} hidden={tab !== key}>
      <span className="lang">bash</span>
      <button type="button" className="copy" aria-label="copy" onClick={() => void copy(code)}>{key === tab && status ? status : "copy"}</button>
      <pre>{code}</pre>
    </div>)}
  </div>;
}

function ScreensCarousel() {
  const [cur, setCur] = useState(0);
  const go = (n: number) => setCur((n + SLIDES.length) % SLIDES.length);
  return <div className="car" id="car">
    <div className="slides">
      {SLIDES.map(([name, title, detail], i) => <div key={name} className={"slide" + (cur === i ? " on" : "")}>
        <picture><source srcSet={"img/" + name + ".webp"} type="image/webp" />
          <img src={"img/" + name + ".png"} width="1200" height="780" loading="lazy" alt={title} /></picture>
        <h3>{title}</h3><p className="d">{detail}</p>
      </div>)}
    </div>
    <div className="ctl">
      <button type="button" id="prev" aria-label="previous" onClick={() => go(cur - 1)}>←</button>
      <span id="count">{cur + 1} / {SLIDES.length}</span>
      <button type="button" id="next" aria-label="next" onClick={() => go(cur + 1)}>→</button>
    </div>
  </div>;
}

function ThemeButton() {
  function toggle() {
    const d = document.documentElement;
    const next = d.dataset.theme === "dark" ? "light" : "dark";
    d.dataset.theme = next;
    try { localStorage.setItem("x-theme", next); } catch { /* private mode */ }
    const meta = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
    if (meta) meta.content = next === "dark" ? "#1b1b1b" : "#f1f1f1";
  }
  return <button className="theme" type="button" id="theme" aria-label="toggle theme" onClick={toggle}>
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
      <circle cx="8" cy="8" r="3" />
      <path d="M8 1.5v1.6M8 12.9v1.6M1.5 8h1.6M12.9 8h1.6M3.4 3.4l1.1 1.1M11.5 11.5l1.1 1.1M3.4 12.6l1.1-1.1M11.5 4.5l1.1-1.1" />
    </svg>
  </button>;
}

export function App() {
  const [active, setActive] = useState("dictation");
  useEffect(() => {
    const observer = new IntersectionObserver(entries => entries.forEach(entry => {
      if (entry.isIntersecting) setActive(entry.target.id === "install" ? "dictation" : entry.target.id);
    }), { rootMargin: "-40% 0px -55% 0px" });
    document.querySelectorAll("main section[id],header[id]").forEach(el => observer.observe(el));
    return () => observer.disconnect();
  }, []);
  return <>
    <SiteNav active={active} />
    
    <header id="top" className="hero">
      <h1>talk. it types. say a word, it acts.</h1>
      <p className="sub">private voice dictation for <code className="c-blue">linux</code> that can also act. speak and it types. start with your wake word and it opens apps and drives your browser.</p>
      <a className="btn" href="https://github.com/0xQuerencia/x/releases/tag/v0.1.0-alpha.1">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round"><path d="M8 2v8m0 0L5 7m3 3 3-3M3 13h10"/></svg>
        download for linux <span className="ver">v0.1.0-alpha.1</span>
      </a>
      <p className="tiny">pre-release · built and tested on arch with hyprland</p>
      <HeroShow />
      <p className="tiny cap">real screenshot of x on arch + hyprland</p>
    </header>
    
    <main>
    <section id="dictation">
      <p className="num"><i>1</i>dictation</p>
      <h2>hold a key, talk, and it types.</h2>
      <p>x is a fork of <a href="https://github.com/anomalyco/hex">hex</a>, the local dictation app. the speech model runs on your own machine, on the gpu when vulkan is there and on the cpu when it isn't.</p>
      <p>it works in any app: browser, editor, chat, terminal. hold the shortcut and let go, or tap it to toggle. the text is pasted where your cursor is.</p>
      <p>the first run downloads a model. after that your voice never leaves the computer, and a name list in <code className="c-purple">vocabulary</code> helps the models that read it (the picker marks which).</p>
    </section>
    
    <section id="agent">
      <p className="num"><i>2</i>the agent</p>
      <h2>say a word, and it does things.</h2>
      <p>on top of dictation there's an agent. it's off until you turn on the labs switch. start a sentence with your wake word (<code className="c-orange">friday</code> by default, or any single word you pick) and x treats it as a command instead of text.</p>
      <p>it can open apps, search, switch workspaces, start <a href="https://opencode.ai">opencode</a> in a fresh folder, and operate a browser you've allowed. for browser work it reads the page through the accessibility tree, acts, and only says done when it can see the result.</p>
      <p>page text is data, not orders. <span className="chip">it can't leave its list of steps</span>, and it never types into password fields or presses send, buy or delete.</p>
      <p><strong>newer on github main:</strong> selected-text actions, voice corrections with reviewable diffs, saved desktop workflows, and an optional hands-free listener. these landed after the downloadable v0.1.0 alpha.1, so build from source to try them until a new verified release is published. <a href="https://github.com/0xQuerencia/x#context-voice-editing-and-workflows">read what's implemented and tested</a>.</p>
    </section>
    
    <section>
      <p className="num"><i>3</i>speech models</p>
      <h2>pick the one that fits how you talk.</h2>
      <p><code className="c-green">parakeet</code> writes while you speak, so there is no wait after you let go. <code className="c-blue">cohere transcribe</code> has the fewest wrong words. <code className="c-orange">whisper large-v3-turbo</code> covers 41 languages and reads your vocabulary list.</p>
      <p>the model picker shows the wait, the error rate and the languages for each one before you download anything. there are more in the list, and you can <span className="chip">add your own gguf file</span>.</p>
    </section>
    
    <section id="install">
      <p className="num"><i>4</i>installation</p>
      <h2>arch first. one script, or the aur.</h2>
      <p><code className="c-blue">x86_64</code> only for now. i've only run it on arch with hyprland, so treat other setups as untested.</p>
      <InstallTabs />
      <p>x keeps its own data in <code className="c-red">~/.local/share/x</code> and runs as <code className="c-teal">x.service</code>, so it can sit beside a normal hex install. bind your dictation key to <code className="c-purple">x dictation toggle</code>.</p>
    </section>
    
    <section>
      <p className="num"><i>5</i>what you get</p>
      <h2>what it can do today.</h2>
      <ul className="feat">
        <li><svg viewBox="0 0 16 16"><path d="M2 8h2M6 5v6M9 3v10M12 6v4M14 8h0"/></svg>local speech to text, pasted into any app</li>
        <li><svg viewBox="0 0 16 16"><rect x="3" y="6" width="10" height="5" rx="1.5"/><path d="M5 8.5h6"/></svg>hold-to-talk or tap-to-toggle, on a shortcut you choose</li>
        <li><svg viewBox="0 0 16 16"><path d="M3 4h10v8H3zM3 7h10"/></svg>a vocabulary list, and a history you can clear</li>
        <li><svg viewBox="0 0 16 16"><path d="M8 2v7M5 6l3 3 3-3M3 13h10"/></svg>nothing is sent anywhere for transcription</li>
        <li><svg viewBox="0 0 16 16"><path d="M3 8h10M8 3v10"/></svg>open and switch apps, jump workspaces on hyprland</li>
        <li><svg viewBox="0 0 16 16"><rect x="2" y="3" width="12" height="10" rx="2"/><path d="M5 7l2 1.5L5 10"/></svg>start opencode in a fresh folder</li>
        <li><svg viewBox="0 0 16 16"><path d="M2 8c2-3 4-4 6-4s4 1 6 4c-2 3-4 4-6 4S4 11 2 8z"/><circle cx="8" cy="8" r="1.6"/></svg>click, type and navigate in a browser you've granted</li>
        <li><svg viewBox="0 0 16 16"><path d="M3 8l3 3 7-7"/></svg>done means it saw the result on screen</li>
        <li><svg viewBox="0 0 16 16"><path d="M8 2l5 2v4c0 3-2 5-5 6-3-1-5-3-5-6V4z"/></svg>risky steps ask first, and a stop button always works</li>
      </ul>
    </section>
    
    <section>
      <p className="num"><i>6</i>voice</p>
      <h2>talk normally. say a word for more.</h2>
      <p>speech without the wake word is just typed, so nothing runs by accident. set your own with <code className="c-purple">x agent wake set jarvis</code>.</p>
      <table>
        <thead><tr><th>you say</th><th>x does</th></tr></thead>
        <tbody>
          <tr><td><kbd>meet me at noon, i'll bring the notes</kbd></td><td>types exactly that</td></tr>
          <tr><td><kbd>friday</kbd> <kbd>open firefox</kbd></td><td>launches firefox</td></tr>
          <tr><td><kbd>friday</kbd> <kbd>search youtube for mars rover</kbd></td><td>opens the search in your browser</td></tr>
          <tr><td><kbd>friday</kbd> <kbd>go to workspace 3</kbd></td><td>switches workspace and confirms it</td></tr>
          <tr><td><kbd>friday work</kbd> <kbd>fix the failing tests</kbd></td><td>runs opencode on your project, with approval</td></tr>
          <tr><td><kbd>friday</kbd> <kbd>go to example dot org in chromium</kbd></td><td>navigates, then looks for the page to prove it</td></tr>
        </tbody>
      </table>
    </section>
    
    <section id="look">
      <h2 className="mid">a closer look</h2>
      <ScreensCarousel />
    </section>
    
    <section id="faq">
      <h2 className="mid">frequently asked questions</h2>
      <div className="faq">
        <details><summary><i>1</i>does my voice go to the cloud?</summary><p>no. transcription runs on your machine. after the first model download it works offline. only if you use the optional agent does a sentence go to a planner model, and you choose that model. an ollama model keeps even that local.</p></details>
        <details><summary><i>2</i>how good is the transcription?</summary><p>it depends on the model. the picker in the app lists each one's wait, wrong-word rate and languages from hex's own measurements. i haven't run my own benchmark, so check the picker for your language.</p></details>
        <details><summary><i>3</i>which languages work?</summary><p>the default, parakeet unified, is english. the picker shows cohere transcribe with 14 languages and whisper large-v3-turbo with 41, plus more models in the list.</p></details>
        <details><summary><i>4</i>do i have to use the agent?</summary><p>no. the agent is off until you turn on the labs switch. without it x is a dictation app, the same as hex.</p></details>
        <details><summary><i>5</i>how do i pick my own wake word?</summary><p>run <code className="c-purple">x agent wake set jarvis</code> with any single word, or <code className="c-purple">add</code> a second one. <code className="c-purple">x agent wake list</code> shows what's set.</p></details>
        <details><summary><i>6</i>what can the browser part see?</summary><p>window titles, button and link names, and page text, read through accessibility. never passwords or form values. that text goes only to a model on your own machine unless you switch on <code className="c-orange">x agent computer remote on</code>.</p></details>
        <details><summary><i>7</i>is this a finished release?</summary><p>no. it's an alpha. the pieces were tested one by one on a single machine.</p></details>
        <details><summary><i>8</i>which systems does it run on?</summary><p>linux, x86_64. i built and tested it on arch with hyprland. there are no mac or windows builds of x.</p></details>
        <details><summary><i>9</i>how is it related to hex?</summary><p>x is a fork. hex's local dictation is the base, and x adds the agent on top. both can be installed together because x keeps its own folders.</p></details>
        <details><summary><i>10</i>how do i stop the agent mid-task?</summary><p>press stop in the agent pane or run <code className="c-purple">x agent stop</code>. browser tasks also end themselves after fifteen steps, three errors or three minutes.</p></details>
      </div>
    </section>
    
    <section className="cta">
      <h2 className="big">stop typing what you can just <em>say</em></h2>
      <a className="btn" href="https://github.com/0xQuerencia/x/releases/tag/v0.1.0-alpha.1">download for linux</a>
      <p className="tiny">x is free, mit licensed</p>
    </section>
    </main>
    
    <footer>
      <a href="https://github.com/0xQuerencia/x">github</a>·<a href="https://github.com/0xQuerencia/x/releases">releases</a>·<a href="https://github.com/0xQuerencia/x/blob/main/LICENSE">license</a>·<a href="https://github.com/0xQuerencia/x/issues">issues</a>
      <p className="tiny">built on hex by christopher langton and contributors</p>
    </footer>
    
    <ThemeButton />
  </>;
}
