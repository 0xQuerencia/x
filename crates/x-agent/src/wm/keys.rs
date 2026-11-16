//! The person's own keyboard shortcuts, read from their compositor config, so X can say
//! what a shortcut does and which one does what ("close the app" is Super+Q here). Read
//! only: nothing in a config is ever executed or sent anywhere, and the text shown is
//! clipped and free of control characters.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(30);
const MAX_FILE: u64 = 256 * 1024;
const MAX_FILES: usize = 40;
const SHOWN: usize = 6;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bind {
    /// Canonical, e.g. `SUPER+Q`.
    pub combo: String,
    /// What it does, in words, e.g. `window kill`.
    pub action: String,
}

fn modifier(token: &str) -> Option<&'static str> {
    Some(match token.to_ascii_lowercase().as_str() {
        "super" | "mod4" | "win" | "windows" | "meta" | "logo" | "$mainmod" | "$mod" => "SUPER",
        "ctrl" | "control" | "primary" => "CTRL",
        "alt" | "mod1" => "ALT",
        "shift" => "SHIFT",
        _ => return None,
    })
}

fn key_name(token: &str) -> String {
    match token.to_ascii_lowercase().as_str() {
        "plus" => "Plus".into(),
        "minus" => "Minus".into(),
        "return" | "enter" => "Return".into(),
        "esc" => "Escape".into(),
        "spacebar" | "space" => "Space".into(),
        t if t.chars().count() == 1 => t.to_ascii_uppercase(),
        t => {
            let mut c = t.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        }
    }
}

/// `SUPER + SHIFT + A`, `super+shift+a` and `Mod4+Shift+a` all become `SUPER+SHIFT+A`.
pub fn canonical(text: &str) -> Option<String> {
    let tokens: Vec<&str> = text
        .split(|c: char| c == '+' || c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    let mut mods: Vec<&'static str> = Vec::new();
    let mut key = None;
    for t in &tokens {
        match modifier(t) {
            Some(m) => mods.push(m),
            None if key.is_none() => key = Some(key_name(t)),
            None => return None,
        }
    }
    let key = key?;
    let mut ordered: Vec<&str> = ["SUPER", "CTRL", "ALT", "SHIFT"]
        .into_iter()
        .filter(|m| mods.contains(m))
        .collect();
    ordered.push(&key);
    Some(ordered.join("+"))
}

fn clip(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.chars().count() > 110 {
        clean.chars().take(109).collect::<String>() + "…"
    } else {
        clean.to_string()
    }
}

fn quoted(s: &str) -> Vec<&str> {
    s.split('"').skip(1).step_by(2).collect()
}

/// `hl.dsp.window.kill()` becomes `window kill`; arguments become words after it.
fn humanize_lua(expr: &str) -> String {
    let expr = expr.trim().trim_end_matches([',', ' ']);
    let Some(rest) = expr.strip_prefix("hl.dsp.") else {
        return clip(expr);
    };
    let path: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
        .collect();
    let mut words = vec![path.replace(['.', '_'], " ")];
    words.extend(quoted(rest).into_iter().map(str::to_string));
    clip(&words.join(" "))
}

fn lua_binds(text: &str) -> Vec<Bind> {
    text.lines()
        .filter_map(|line| {
            let after = line.split("hl.bind(").nth(1)?;
            let mut parts = after.splitn(3, '"');
            // A shortcut written with variables or code cannot be read, so it is skipped.
            parts.next().filter(|before| before.trim().is_empty())?;
            let combo = canonical(parts.next()?)?;
            let rest = parts.next()?.trim_start().strip_prefix(',')?.trim();
            // Trailing options and the call's own closing parenthesis are not the action.
            let action = rest.split(", {").next().unwrap_or(rest);
            let action = action.strip_suffix(')').unwrap_or(action);
            Some(Bind {
                combo,
                action: humanize_lua(action),
            })
        })
        .collect()
}

fn hyprlang_binds(text: &str) -> Vec<Bind> {
    let vars: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix('$')?.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let fill = |s: &str| {
        vars.iter().fold(s.to_string(), |acc, (k, v)| {
            acc.replace(&format!("${k}"), v)
        })
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (head, body) = line.split_once('=')?;
            let head = head.trim();
            if !head.starts_with("bind") || head.chars().any(|c| !c.is_ascii_alphabetic()) {
                return None;
            }
            let mut f = body.splitn(4, ',').map(str::trim);
            let (mods, key, dispatcher) = (f.next()?, f.next()?, f.next()?);
            let combo = canonical(&format!("{} {}", fill(mods), fill(key)))?;
            let arg = f.next().unwrap_or("");
            Some(Bind {
                combo,
                action: clip(&format!("{dispatcher} {}", fill(arg))),
            })
        })
        .collect()
}

fn sway_binds(text: &str) -> Vec<Bind> {
    let vars: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("set ")?.split_once(' '))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("bindsym ")?;
            let mut it = rest.split_whitespace().filter(|t| !t.starts_with("--"));
            let combo = it.next()?;
            let combo = vars
                .iter()
                .fold(combo.to_string(), |acc, (k, v)| acc.replace(k, v));
            Some(Bind {
                combo: canonical(&combo)?,
                action: clip(&it.collect::<Vec<_>>().join(" ")),
            })
        })
        .collect()
}

pub fn parse(file: &Path, text: &str) -> Vec<Bind> {
    match file.extension().and_then(|e| e.to_str()) {
        Some("lua") => lua_binds(text),
        Some("conf") if file.to_string_lossy().contains("hypr") => hyprlang_binds(text),
        _ => sway_binds(text),
    }
}

fn config_files() -> Vec<PathBuf> {
    let Some(base) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
    else {
        return Vec::new();
    };
    let mut files = vec![base.join("sway/config"), base.join("i3/config")];
    for dir in [
        base.join("hypr"),
        base.join("hypr/lua"),
        base.join("hypr/hyprland"),
    ] {
        if let Ok(read) = std::fs::read_dir(dir) {
            let mut found: Vec<PathBuf> = read
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "lua" || e == "conf"))
                .collect();
            found.sort();
            files.extend(found);
        }
    }
    files
}

/// Read active keybindings from the compositor when available; a parsed
/// config may be stale after runtime remaps. Anonymous Lua dispatchers have
/// no human-readable action and are intentionally skipped.
fn parse_live_hyprland(text: &str) -> Vec<Bind> {
    let Ok(list) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(list) = list.as_array() else {
        return Vec::new();
    };
    list.iter()
        .take(512)
        .filter_map(|entry| {
            let mask = entry.get("modmask")?.as_u64()?;
            let key = entry.get("key")?.as_str()?.trim();
            let action = entry.get("dispatcher")?.as_str()?.trim();
            let arg = entry
                .get("arg")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if key.is_empty()
                || key.chars().count() > 64
                || action.is_empty()
                || action.starts_with("__")
                || entry
                    .get("submap")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            {
                return None;
            }
            // Hyprland uses XKB modifier masks: Shift=1, Ctrl=4, Alt=8,
            // Mod4/Super=64. Ignore unknown masks rather than guessing.
            if mask & !(1 | 4 | 8 | 64) != 0 {
                return None;
            }
            let mods = [
                ((mask & 64 != 0), "SUPER"),
                ((mask & 4 != 0), "CTRL"),
                ((mask & 8 != 0), "ALT"),
                ((mask & 1 != 0), "SHIFT"),
            ];
            let mut parts: Vec<&str> = mods.iter().filter(|(on, _)| *on).map(|(_, m)| *m).collect();
            parts.push(key);
            let combo = canonical(&parts.join("+"))?;
            Some(Bind {
                combo,
                action: clip(&format!("{action} {arg}")),
            })
        })
        .collect()
}

fn live_hyprland() -> Vec<Bind> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return Vec::new();
    }
    let Ok(output) = std::process::Command::new("hyprctl")
        .args(["binds", "-j"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() || output.stdout.len() > MAX_FILE as usize {
        return Vec::new();
    }
    std::str::from_utf8(&output.stdout)
        .map(parse_live_hyprland)
        .unwrap_or_default()
}

fn read_all() -> Vec<Bind> {
    let mut out: Vec<Bind> = live_hyprland();
    for file in config_files().into_iter().take(MAX_FILES) {
        let ok = std::fs::metadata(&file).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE);
        if let Some(text) = ok.then(|| std::fs::read_to_string(&file).ok()).flatten() {
            for bind in parse(&file, &text) {
                // Registered live binds win over a guessed config file; a
                // Lua callback without a named action still uses config text.
                if !out.iter().any(|current| current.combo == bind.combo) {
                    out.push(bind);
                }
            }
        }
    }
    out
}

static CACHE: Mutex<Option<(Instant, Vec<Bind>)>> = Mutex::new(None);

/// The shortcuts in the person's config, re-read at most every half minute.
pub fn load() -> Vec<Bind> {
    let Ok(mut slot) = CACHE.lock() else {
        return read_all();
    };
    match &*slot {
        Some((at, binds)) if at.elapsed() < TTL => binds.clone(),
        _ => {
            let binds = read_all();
            *slot = Some((Instant::now(), binds.clone()));
            binds
        }
    }
}

/// Plain English words that mean the same as the words a config uses.
fn synonyms(word: &str) -> &'static [&'static str] {
    match word {
        "close" | "closes" | "quit" | "exit" | "kill" | "kills" => {
            &["close", "kill", "killactive", "quit"]
        }
        "maximize" | "fullscreen" | "maximise" => &["fullscreen", "maximize"],
        "float" | "floating" => &["float", "floating"],
        "workspace" | "desktop" => &["workspace"],
        "screenshot" => &["screenshot", "grim", "screencap"],
        "lock" => &["lock", "lockscreen"],
        "volume" => &["volume", "audio", "pamixer", "wpctl"],
        "brightness" => &["brightness", "bright"],
        "launcher" | "menu" | "run" => &["launcher", "rofi", "wofi", "drun", "menu"],
        _ => &[],
    }
}

const STOP: [&str; 24] = [
    "what",
    "whats",
    "does",
    "do",
    "is",
    "the",
    "a",
    "an",
    "my",
    "key",
    "keys",
    "keybind",
    "keybinds",
    "shortcut",
    "shortcuts",
    "bound",
    "to",
    "for",
    "how",
    "i",
    "which",
    "when",
    "press",
    "can",
];

/// Shortcuts that answer `query`: a key combination, or the words of what it does.
pub fn find(binds: &[Bind], query: &str) -> Vec<Bind> {
    let spoken = query.to_ascii_lowercase();
    let combo_words: Vec<&str> = spoken
        .split(|c: char| c.is_whitespace() || c == '+')
        .filter(|w| !w.is_empty() && *w != "plus" && !STOP.contains(w))
        .collect();
    if let Some(combo) = canonical(&combo_words.join(" ")).filter(|c| c.contains('+')) {
        return binds.iter().filter(|b| b.combo == combo).cloned().collect();
    }
    let words: Vec<&str> = spoken
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !STOP.contains(w))
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    binds
        .iter()
        .filter(|b| {
            let action = b.action.to_ascii_lowercase();
            words
                .iter()
                .all(|w| action.contains(w) || synonyms(w).iter().any(|s| action.contains(s)))
        })
        .cloned()
        .collect()
}

fn spaced(combo: &str) -> String {
    combo.replace('+', " + ")
}

/// A sentence answering a spoken question about the person's shortcuts.
pub fn answer(binds: &[Bind], query: &str) -> String {
    if binds.is_empty() {
        return "I found no keyboard shortcuts in your window manager config".into();
    }
    let q = clip(query);
    if q.is_empty() {
        return format!(
            "You have {} shortcuts. Ask what one does (\"what does super q do\") or which one does something (\"what closes a window\")",
            binds.len()
        );
    }
    let hits = find(binds, &q);
    if hits.is_empty() {
        return format!("No shortcut of yours matches \"{q}\"");
    }
    let more = hits.len().saturating_sub(SHOWN);
    let mut text = hits
        .iter()
        .take(SHOWN)
        .map(|b| format!("{}: {}", spaced(&b.combo), b.action))
        .collect::<Vec<_>>()
        .join("; ");
    if more > 0 {
        text.push_str(&format!("; and {more} more"));
    }
    text
}

/// Your own shortcut for an action, when the config has one, e.g. ` (your shortcut: SUPER + Q)`.
pub fn hint(word: &str, binds: &[Bind]) -> String {
    find(binds, word)
        .first()
        .map(|b| format!(" (your shortcut: {})", spaced(&b.combo)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUA: &str = r#"
        hl.bind("SUPER + Q", hl.dsp.window.kill())
        hl.bind("SUPER + 1", hl.dsp.focus({ workspace = "1" }))
        hl.bind("SUPER + Return", hl.dsp.exec_cmd("foot"))
        hl.bind("XF86AudioPlay", hl.dsp.exec_cmd("playerctl play-pause"), { locked = true })
        hl.bind(modkey .. " + X", hl.dsp.window.kill())
    "#;

    fn binds() -> Vec<Bind> {
        lua_binds(LUA)
    }

    #[test]
    fn live_hyprland_bindings_use_registered_action_not_config_guess() {
        let data = serde_json::json!([
            {"modmask":64,"key":"Q","dispatcher":"killactive","arg":"","submap":""},
            {"modmask":68,"key":"RETURN","dispatcher":"exec","arg":"foot","submap":""},
            {"modmask":64,"key":"R","dispatcher":"__lua","arg":"4","submap":""},
            {"modmask":64,"key":"X","dispatcher":"exec","arg":"hidden","submap":"resize"},
            {"modmask":0,"key":"Print","dispatcher":"exec","arg":"grim","submap":""}
        ]);
        let binds = parse_live_hyprland(&data.to_string());
        assert_eq!(binds.len(), 3);
        assert_eq!(binds[0].combo, "SUPER+Q");
        assert_eq!(binds[0].action, "killactive");
        assert_eq!(binds[1].combo, "SUPER+CTRL+Return");
        assert_eq!(binds[1].action, "exec foot");
        assert_eq!(binds[2].combo, "Print");
        assert!(parse_live_hyprland("{broken json").is_empty());
    }

    #[test]
    fn reads_lua_binds_and_skips_what_it_cannot_resolve() {
        let b = binds();
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].combo, "SUPER+Q");
        assert_eq!(b[0].action, "window kill");
        assert_eq!(b[1].action, "focus 1");
        assert_eq!(b[2].action, "exec cmd foot");
    }

    #[test]
    fn reads_hyprlang_and_sway_binds_with_variables() {
        let h = hyprlang_binds(
            "$mod = SUPER\nbind = $mod, Q, killactive,\nbindm = $mod, mouse:272, movewindow\n",
        );
        assert_eq!(h[0].combo, "SUPER+Q");
        assert_eq!(h[0].action, "killactive");
        let s = sway_binds("set $mod Mod4\nbindsym $mod+Shift+q kill\n");
        assert_eq!(s[0].combo, "SUPER+SHIFT+Q");
        assert_eq!(s[0].action, "kill");
    }

    #[test]
    fn closing_means_the_close_shortcut_whatever_it_is_called() {
        let b = binds();
        assert_eq!(find(&b, "close a window")[0].combo, "SUPER+Q");
        assert_eq!(
            find(&b, "what does super plus q do")[0].action,
            "window kill"
        );
        assert_eq!(find(&b, "super q")[0].combo, "SUPER+Q");
        assert!(find(&b, "teleport").is_empty());
        assert_eq!(hint("close", &b), " (your shortcut: SUPER + Q)");
    }

    #[test]
    fn answers_are_short_and_honest_when_nothing_matches() {
        let b = binds();
        assert!(answer(&b, "close").contains("SUPER + Q"));
        assert!(answer(&b, "teleport").starts_with("No shortcut"));
        assert!(answer(&[], "close").contains("no keyboard shortcuts"));
        assert!(answer(&b, "").contains("4 shortcuts"));
    }

    #[test]
    fn combos_are_canonical() {
        assert_eq!(canonical("Mod4+shift+a").unwrap(), "SUPER+SHIFT+A");
        assert_eq!(canonical("ctrl alt delete").unwrap(), "CTRL+ALT+Delete");
        assert!(canonical("a b").is_none());
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// `cargo test -p x-agent --lib keys::live -- --ignored --nocapture` on a real desktop.
    #[test]
    #[ignore = "reads the real compositor config"]
    fn reads_the_real_config() {
        let binds = load();
        println!("{} shortcuts", binds.len());
        println!("close -> {}", answer(&binds, "close a window"));
        println!("super q -> {}", answer(&binds, "what does super q do"));
        assert!(!binds.is_empty());
    }
}
