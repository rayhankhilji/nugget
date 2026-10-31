//! Double Tap preferences: the trigger, a provider per app and the excluded apps, kept in
//! `doubletap.json` under the app's config directory. Validated on every save.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nugget_core::ipc::AppInfo;
use nugget_hotkey::{Modifier, Trigger};
use serde::{Deserialize, Serialize};

/// A modifier as the settings page names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifierId {
    LeftCommand,
    RightCommand,
    LeftControl,
    RightControl,
    LeftOption,
    RightOption,
    LeftShift,
    RightShift,
}

impl ModifierId {
    pub const ALL: [ModifierId; 8] = [
        Self::LeftCommand,
        Self::RightCommand,
        Self::LeftControl,
        Self::RightControl,
        Self::LeftOption,
        Self::RightOption,
        Self::LeftShift,
        Self::RightShift,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeftCommand => "left_command",
            Self::RightCommand => "right_command",
            Self::LeftControl => "left_control",
            Self::RightControl => "right_control",
            Self::LeftOption => "left_option",
            Self::RightOption => "right_option",
            Self::LeftShift => "left_shift",
            Self::RightShift => "right_shift",
        }
    }
}

impl From<ModifierId> for Modifier {
    fn from(m: ModifierId) -> Self {
        match m {
            ModifierId::LeftCommand => Modifier::LeftCommand,
            ModifierId::RightCommand => Modifier::RightCommand,
            ModifierId::LeftControl => Modifier::LeftControl,
            ModifierId::RightControl => Modifier::RightControl,
            ModifierId::LeftOption => Modifier::LeftOption,
            ModifierId::RightOption => Modifier::RightOption,
            ModifierId::LeftShift => Modifier::LeftShift,
            ModifierId::RightShift => Modifier::RightShift,
        }
    }
}

impl From<Modifier> for ModifierId {
    fn from(m: Modifier) -> Self {
        match m {
            Modifier::LeftCommand => ModifierId::LeftCommand,
            Modifier::RightCommand => ModifierId::RightCommand,
            Modifier::LeftControl => ModifierId::LeftControl,
            Modifier::RightControl => ModifierId::RightControl,
            Modifier::LeftOption => ModifierId::LeftOption,
            Modifier::RightOption => ModifierId::RightOption,
            Modifier::LeftShift => ModifierId::LeftShift,
            Modifier::RightShift => ModifierId::RightShift,
        }
    }
}

/// The trigger as stored and sent to the settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerSpec {
    DoubleTap {
        modifier: ModifierId,
        window_ms: u32,
    },
    Chord {
        chord: String,
    },
}

impl From<&TriggerSpec> for Trigger {
    fn from(t: &TriggerSpec) -> Self {
        match t {
            TriggerSpec::DoubleTap {
                modifier,
                window_ms,
            } => Trigger::DoubleTap {
                modifier: (*modifier).into(),
                window_ms: *window_ms,
            },
            TriggerSpec::Chord { chord } => Trigger::Chord(chord.clone()),
        }
    }
}

impl From<&Trigger> for TriggerSpec {
    fn from(t: &Trigger) -> Self {
        match t {
            Trigger::DoubleTap {
                modifier,
                window_ms,
            } => TriggerSpec::DoubleTap {
                modifier: (*modifier).into(),
                window_ms: *window_ms,
            },
            Trigger::Chord(c) => TriggerSpec::Chord { chord: c.clone() },
        }
    }
}

impl TriggerSpec {
    pub fn recommended() -> Self {
        (&nugget_hotkey::recommended_trigger()).into()
    }

    pub fn is_double_tap(&self) -> bool {
        matches!(self, TriggerSpec::DoubleTap { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DoubleTapSettings {
    pub enabled: bool,
    pub trigger: TriggerSpec,
    /// App id or page host -> provider id.
    pub app_providers: BTreeMap<String, String>,
    /// App ids, app names or page hosts (subdomains included) never drafted into. Entries in
    /// the deny-rule syntax of `nugget_a11y` (`id:`, `name:`, `name~`, `host:`, `host~`) work too.
    pub excluded_apps: Vec<String>,
    /// Allow drafting into terminal emulators ([`TERMINALS`]); off by default, because text
    /// inserted into a shell can run as a command.
    pub allow_terminals: bool,
    /// Read ahead: while a reply field keeps focus, read its context and ask the daemon to
    /// prefill a local model's cache with it, so the trigger only reads the remainder. Local
    /// providers only; see [`super::readahead`] for exactly what is read and sent. On by default.
    #[serde(default = "default_read_ahead")]
    pub read_ahead: bool,
    /// Keep reading ahead while the computer runs on battery. On by default; low power mode
    /// (battery saver) always pauses read-ahead, whatever this says.
    #[serde(default = "default_read_ahead_on_battery")]
    pub read_ahead_on_battery: bool,
}

/// The default for [`DoubleTapSettings::read_ahead`] (also used for files written before the
/// setting existed).
pub const fn default_read_ahead() -> bool {
    true
}

/// The default for [`DoubleTapSettings::read_ahead_on_battery`] (also used for older files).
pub const fn default_read_ahead_on_battery() -> bool {
    true
}

impl Default for DoubleTapSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger: TriggerSpec::recommended(),
            app_providers: BTreeMap::new(),
            excluded_apps: default_excluded(),
            allow_terminals: false,
            read_ahead: default_read_ahead(),
            read_ahead_on_battery: default_read_ahead_on_battery(),
        }
    }
}

/// Password managers and banking, prefilled in the excluded list.
pub const DEFAULT_EXCLUDED: &[&str] = &[
    // Password managers and credential stores.
    "1Password",
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "Bitwarden",
    "com.bitwarden.desktop",
    "KeePassXC",
    "org.keepassxc.KeePassXC",
    "KeePass",
    "LastPass",
    "Dashlane",
    "Enpass",
    "in.sinew.Enpass-Desktop",
    "Enpass.exe",
    "NordPass",
    "com.nordsec.nordpass",
    "NordPass.exe",
    "Proton Pass",
    "me.proton.pass.electron",
    "ProtonPass.exe",
    "Proton Pass.exe",
    "proton-pass",
    "pass.proton.me",
    "nordpass.com",
    "Keeper",
    "RoboForm",
    "Keychain Access",
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "Passwords and Keys",
    "org.gnome.seahorse.Application",
    "KWalletManager",
    "1password.com",
    "bitwarden.com",
    "lastpass.com",
    "dashlane.com",
    "passwords.google.com",
    // Banking, payments and brokerage.
    "name~bank",
    "host~bank",
    "PayPal",
    "Venmo",
    "Revolut",
    "Monzo",
    "Wise",
    "Cash App",
    "Coinbase",
    "Robinhood",
    "chase.com",
    "wellsfargo.com",
    "citi.com",
    "capitalone.com",
    "americanexpress.com",
    "paypal.com",
    "venmo.com",
    "revolut.com",
    "monzo.com",
    "n26.com",
    "wise.com",
    "schwab.com",
    "fidelity.com",
    "vanguard.com",
    "robinhood.com",
    "coinbase.com",
    "barclays.co.uk",
    "hsbc.com",
    "santander.com",
    "lloydsbank.com",
];

/// Terminal emulators: refused unless `allow_terminals` is set. Kept apart from
/// `excluded_apps`, so editing or resetting that list never drops them.
pub const TERMINALS: &[&str] = &[
    // macOS
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.mitchellh.ghostty",
    "net.kovidgoyal.kitty",
    "org.alacritty",
    "io.alacritty",
    "com.github.wez.wezterm",
    "dev.warp.Warp-Stable",
    "Terminal",
    "iTerm2",
    "iTerm",
    "Ghostty",
    "Warp",
    // Windows
    "WindowsTerminal.exe",
    "OpenConsole.exe",
    "conhost.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "powershell_ise.exe",
    "wsl.exe",
    "mintty.exe",
    "Windows Terminal",
    "Command Prompt",
    "Windows PowerShell",
    "PowerShell",
    // Linux
    "org.gnome.Terminal",
    "gnome-terminal",
    "gnome-terminal-server",
    "org.gnome.Console",
    "kgx",
    "org.gnome.Ptyxis",
    "org.kde.konsole",
    "konsole",
    "Alacritty",
    "kitty",
    "org.wezfurlong.wezterm",
    "wezterm",
    "xterm",
    "urxvt",
    "xfce4-terminal",
    "org.xfce.terminal",
    "tilix",
    "com.gexperts.Tilix",
    "terminator",
    "foot",
    "GNOME Terminal",
    "Console",
    "Konsole",
    "WezTerm",
];

/// Whether `app` is a terminal emulator (by app id or app name, case-insensitively).
pub fn is_terminal(app: &AppInfo) -> bool {
    let probe = AppInfo {
        url: None,
        ..app.clone()
    };
    TERMINALS.iter().any(|t| entry_matches(t, &probe))
}

pub fn default_excluded() -> Vec<String> {
    DEFAULT_EXCLUDED.iter().map(|s| (*s).to_owned()).collect()
}

pub const MAX_EXCLUDED: usize = 500;
pub const MAX_PROVIDERS: usize = 200;
pub const MAX_ENTRY_LEN: usize = 256;
pub const MAX_CHORD_LEN: usize = 64;

fn clean_entry(s: &str, what: &str) -> Result<String, String> {
    let s = s.trim();
    if s.chars().count() > MAX_ENTRY_LEN {
        return Err(format!("{what} is longer than {MAX_ENTRY_LEN} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("{what} contains control characters"));
    }
    Ok(s.to_owned())
}

impl DoubleTapSettings {
    /// Check and normalise: the trigger parses and its window is in range, lists are bounded,
    /// entries trimmed, empty ones dropped, duplicates removed. A trigger that this session
    /// cannot observe (a double tap on Wayland) is accepted here and reported by the status.
    pub fn validated(mut self) -> Result<Self, String> {
        match &mut self.trigger {
            TriggerSpec::DoubleTap { window_ms, .. } => {
                if !nugget_hotkey::WINDOW_MS_RANGE.contains(window_ms) {
                    return Err(format!(
                        "the double-tap window must be between {} and {} ms",
                        nugget_hotkey::WINDOW_MS_RANGE.start(),
                        nugget_hotkey::WINDOW_MS_RANGE.end()
                    ));
                }
            }
            TriggerSpec::Chord { chord } => {
                *chord = chord.trim().to_owned();
                if chord.is_empty() || chord.len() > MAX_CHORD_LEN {
                    return Err("enter a shortcut such as Ctrl+Alt+Space".into());
                }
                nugget_hotkey::parse_chord(chord).map_err(|e| e.to_string())?;
            }
        }
        if let Err(nugget_core::Error::Invalid(e)) = nugget_hotkey::check(&(&self.trigger).into()) {
            return Err(e);
        }
        if self.app_providers.len() > MAX_PROVIDERS {
            return Err(format!("at most {MAX_PROVIDERS} per-app providers"));
        }
        let mut providers = BTreeMap::new();
        for (app, provider) in std::mem::take(&mut self.app_providers) {
            let app = clean_entry(&app, "an app")?.to_lowercase();
            let provider = clean_entry(&provider, "a provider id")?;
            if app.is_empty() || provider.is_empty() {
                continue;
            }
            providers.insert(app, provider);
        }
        self.app_providers = providers;
        if self.excluded_apps.len() > MAX_EXCLUDED {
            return Err(format!("at most {MAX_EXCLUDED} excluded apps"));
        }
        let mut seen = std::collections::HashSet::new();
        let mut excluded = Vec::with_capacity(self.excluded_apps.len());
        for e in std::mem::take(&mut self.excluded_apps) {
            let e = clean_entry(&e, "an excluded app")?;
            if !e.is_empty() && seen.insert(e.to_lowercase()) {
                excluded.push(e);
            }
        }
        self.excluded_apps = excluded;
        Ok(self)
    }
}

pub fn file_in(config_dir: &Path) -> PathBuf {
    config_dir.join("doubletap.json")
}

/// Missing or unreadable settings fall back to the defaults; an invalid file is ignored.
pub fn load(path: &Path) -> DoubleTapSettings {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<DoubleTapSettings>(&bytes)
            .map_err(|e| e.to_string())
            .and_then(DoubleTapSettings::validated)
            .unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "ignoring invalid Double Tap settings");
                DoubleTapSettings::default()
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DoubleTapSettings::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read Double Tap settings");
            DoubleTapSettings::default()
        }
    }
}

/// Write via a temporary file and rename.
pub fn save(path: &Path, settings: &DoubleTapSettings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

// --- matching --------------------------------------------------------------------------------

/// The lower-cased host of a page URL.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next()?.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn host_matches(host: &str, domain: &str) -> bool {
    let domain = domain.trim_start_matches('.');
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

/// An app id without the platform decorations (`.exe`, `.desktop`), lower-cased.
fn normalize_id(id: &str) -> String {
    let id = id.trim().to_lowercase();
    let id = id.strip_suffix(".exe").unwrap_or(&id);
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    id.to_owned()
}

fn is_rule_syntax(entry: &str) -> bool {
    ["id:", "name:", "name~", "host:", "host~", "id~"]
        .iter()
        .any(|p| entry.len() > p.len() && entry[..p.len()].eq_ignore_ascii_case(p))
}

/// Whether `app` matches one excluded entry: the app id, the app name or the page host
/// (subdomains included), case-insensitively; or a `nugget_a11y` deny rule.
pub fn entry_matches(entry: &str, app: &AppInfo) -> bool {
    let entry = entry.trim();
    if entry.is_empty() {
        return false;
    }
    if is_rule_syntax(entry) {
        return nugget_a11y::DenyRule::parse(entry).is_some_and(|r| r.matches(app));
    }
    let e = entry.to_lowercase();
    if app
        .id
        .as_deref()
        .is_some_and(|id| normalize_id(id) == normalize_id(&e))
    {
        return true;
    }
    if !app.name.trim().is_empty() && app.name.trim().to_lowercase() == e {
        return true;
    }
    app.url
        .as_deref()
        .and_then(host_of)
        .is_some_and(|h| host_matches(&h, &e))
}

/// Whether drafting is excluded in `app`, by the user's list or by the accessibility layer's
/// built-in deny list (which also hides the surroundings of those apps).
pub fn is_excluded(app: &AppInfo, excluded: &[String]) -> bool {
    excluded.iter().any(|e| entry_matches(e, app))
        || nugget_a11y::DenyList::defaults().is_denied(app)
}

/// The provider configured for `app`: by app id first, then by page host (the most specific
/// configured domain wins), then by app name.
pub fn provider_for(app: &AppInfo, providers: &BTreeMap<String, String>) -> Option<String> {
    if let Some(id) = app.id.as_deref() {
        let id = normalize_id(id);
        if let Some(p) = providers.iter().find(|(k, _)| normalize_id(k) == id) {
            return Some(p.1.clone());
        }
    }
    if let Some(host) = app.url.as_deref().and_then(host_of) {
        let best = providers
            .iter()
            .filter(|(k, _)| host_matches(&host, &k.to_lowercase()))
            .max_by_key(|(k, _)| k.len());
        if let Some((_, p)) = best {
            return Some(p.clone());
        }
    }
    let name = app.name.trim().to_lowercase();
    if !name.is_empty() {
        return providers
            .iter()
            .find(|(k, _)| k.to_lowercase() == name)
            .map(|(_, p)| p.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: Option<&str>, name: &str, url: Option<&str>) -> AppInfo {
        AppInfo {
            id: id.map(Into::into),
            name: name.into(),
            window_title: None,
            url: url.map(Into::into),
        }
    }

    #[test]
    fn defaults_and_partial_files() {
        let s = DoubleTapSettings::default();
        assert!(s.enabled);
        assert_eq!(s.trigger, TriggerSpec::recommended());
        assert!(s.app_providers.is_empty());
        assert!(s.excluded_apps.iter().any(|e| e == "1Password"));
        assert!(s.excluded_apps.iter().any(|e| e == "chase.com"));
        let s: DoubleTapSettings = serde_json::from_str(r#"{"enabled":false}"#).unwrap();
        assert!(!s.enabled);
        assert_eq!(s.excluded_apps, default_excluded());
        // Files written before read-ahead existed get its default.
        assert!(s.read_ahead);
        assert!(DoubleTapSettings::default().read_ahead);
        assert_eq!(
            serde_json::to_value(DoubleTapSettings::default()).unwrap()["read_ahead"],
            serde_json::json!(true)
        );
        assert!(s.read_ahead_on_battery);
        assert_eq!(
            serde_json::to_value(DoubleTapSettings::default()).unwrap()["read_ahead_on_battery"],
            serde_json::json!(true)
        );
        // The defaults validate unchanged.
        assert_eq!(
            DoubleTapSettings::default().validated().unwrap(),
            DoubleTapSettings::default()
        );
    }

    #[test]
    fn trigger_serde_matches_the_contract() {
        let t = TriggerSpec::DoubleTap {
            modifier: ModifierId::RightCommand,
            window_ms: 350,
        };
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            serde_json::json!({"kind":"double_tap","modifier":"right_command","window_ms":350})
        );
        let c: TriggerSpec =
            serde_json::from_str(r#"{"kind":"chord","chord":"Ctrl+Alt+Space"}"#).unwrap();
        assert_eq!(Trigger::from(&c), Trigger::Chord("Ctrl+Alt+Space".into()));
        for m in ModifierId::ALL {
            assert_eq!(serde_json::to_value(m).unwrap(), m.as_str());
            assert_eq!(ModifierId::from(Modifier::from(m)), m);
        }
    }

    #[test]
    fn validation() {
        let base = DoubleTapSettings::default();
        let bad_window = DoubleTapSettings {
            trigger: TriggerSpec::DoubleTap {
                modifier: ModifierId::LeftShift,
                window_ms: 50,
            },
            ..base.clone()
        };
        assert!(bad_window.validated().is_err());
        for chord in ["", "Ctrl+", "A", &"Ctrl+".repeat(20)] {
            let s = DoubleTapSettings {
                trigger: TriggerSpec::Chord {
                    chord: chord.into(),
                },
                ..base.clone()
            };
            assert!(s.validated().is_err(), "{chord:?}");
        }
        let ok = DoubleTapSettings {
            trigger: TriggerSpec::Chord {
                chord: " Ctrl+Alt+Space ".into(),
            },
            app_providers: [
                (
                    " Com.Tinyspeck.SlackMacGap ".to_string(),
                    " local ".to_string(),
                ),
                ("".into(), "x".into()),
            ]
            .into_iter()
            .collect(),
            excluded_apps: vec![" Foo ".into(), "foo".into(), "".into(), "Bar".into()],
            ..base.clone()
        }
        .validated()
        .unwrap();
        assert_eq!(
            ok.trigger,
            TriggerSpec::Chord {
                chord: "Ctrl+Alt+Space".into()
            }
        );
        assert_eq!(
            ok.app_providers
                .get("com.tinyspeck.slackmacgap")
                .map(String::as_str),
            Some("local")
        );
        assert_eq!(ok.app_providers.len(), 1);
        assert_eq!(ok.excluded_apps, vec!["Foo".to_string(), "Bar".to_string()]);

        let too_many = DoubleTapSettings {
            excluded_apps: (0..=MAX_EXCLUDED).map(|i| format!("app{i}")).collect(),
            ..base.clone()
        };
        assert!(too_many.validated().is_err());
        let too_long = DoubleTapSettings {
            excluded_apps: vec!["x".repeat(MAX_ENTRY_LEN + 1)],
            ..base.clone()
        };
        assert!(too_long.validated().is_err());
        let control = DoubleTapSettings {
            excluded_apps: vec!["a\u{7}b".into()],
            ..base
        };
        assert!(control.validated().is_err());
    }

    #[test]
    fn onboarding_choices_validate() {
        // Onboarding saves through `doubletap_settings_set`: its chords and tap speeds must pass.
        for chord in ["Alt+Space", "CmdOrCtrl+Shift+Space", "CmdOrCtrl+Alt+K"] {
            let s = DoubleTapSettings {
                trigger: TriggerSpec::Chord {
                    chord: chord.into(),
                },
                ..DoubleTapSettings::default()
            };
            assert!(s.validated().is_ok(), "{chord}");
        }
        for window_ms in [250, 350, 500] {
            for modifier in ModifierId::ALL {
                let s = DoubleTapSettings {
                    trigger: TriggerSpec::DoubleTap {
                        modifier,
                        window_ms,
                    },
                    ..DoubleTapSettings::default()
                };
                assert!(s.validated().is_ok(), "{modifier:?} {window_ms}");
            }
        }
    }

    #[test]
    fn excluded_matching() {
        let list = default_excluded();
        assert!(is_excluded(
            &app(Some("com.bitwarden.desktop"), "Bitwarden", None),
            &list
        ));
        assert!(is_excluded(&app(None, "1PASSWORD", None), &list));
        assert!(is_excluded(
            &app(Some("KeePassXC.exe"), "KeePassXC", None),
            &list
        ));
        assert!(is_excluded(
            &app(
                Some("firefox"),
                "Firefox",
                Some("https://secure.chase.com/x")
            ),
            &list
        ));
        assert!(is_excluded(
            &app(
                Some("firefox"),
                "Firefox",
                Some("https://my.examplebank.com/")
            ),
            &list
        ));
        assert!(!is_excluded(
            &app(Some("com.tinyspeck.slackmacgap"), "Slack", None),
            &list
        ));
        assert!(!is_excluded(
            &app(
                Some("firefox"),
                "Firefox",
                Some("https://mail.example.com/chase.com")
            ),
            &list
        ));
        // User entries: id, name, host with subdomains, and rule syntax.
        let user = vec![
            "org.signal.Signal".to_string(),
            "intranet.corp".to_string(),
            "name~secret".to_string(),
        ];
        assert!(is_excluded(
            &app(Some("ORG.SIGNAL.SIGNAL"), "Signal", None),
            &user
        ));
        assert!(is_excluded(
            &app(None, "Edge", Some("http://wiki.intranet.corp:8080/x")),
            &user
        ));
        assert!(is_excluded(&app(None, "My Secret Notes", None), &user));
        assert!(!is_excluded(
            &app(None, "Edge", Some("http://notintranet.corp/")),
            &user
        ));
    }

    #[test]
    fn password_managers_from_the_security_review_are_excluded() {
        let list = default_excluded();
        for a in [
            app(Some("me.proton.pass.electron"), "Proton Pass", None),
            app(Some("ProtonPass.exe"), "", None),
            app(Some("proton-pass.desktop"), "", None),
            app(Some("in.sinew.Enpass-Desktop"), "", None),
            app(Some("Enpass.exe"), "", None),
            app(Some("com.nordsec.nordpass"), "", None),
            app(Some("nordpass"), "", None),
            app(
                Some("firefox"),
                "Firefox",
                Some("https://pass.proton.me/u/0"),
            ),
            app(
                Some("firefox"),
                "Firefox",
                Some("https://app.nordpass.com/"),
            ),
        ] {
            assert!(list.iter().any(|e| entry_matches(e, &a)), "{a:?}");
        }
    }

    #[test]
    fn terminals_are_a_separate_built_in_list() {
        for a in [
            app(Some("com.apple.Terminal"), "Terminal", None),
            app(Some("com.googlecode.iterm2"), "iTerm2", None),
            app(Some("WindowsTerminal.exe"), "", None),
            app(Some("conhost.exe"), "", None),
            app(Some("cmd.exe"), "", None),
            app(Some("powershell.exe"), "", None),
            app(Some("pwsh.exe"), "", None),
            app(Some("org.gnome.Terminal.desktop"), "", None),
            app(Some("org.kde.konsole"), "", None),
            app(None, "Alacritty", None),
            app(Some("kitty"), "kitty", None),
            app(None, "WezTerm", None),
            app(Some("com.mitchellh.ghostty"), "Ghostty", None),
        ] {
            assert!(is_terminal(&a), "{a:?}");
        }
        assert!(!is_terminal(&app(
            Some("com.tinyspeck.slackmacgap"),
            "Slack",
            None
        )));
        // A page whose host happens to be named like a terminal is not a terminal.
        assert!(!is_terminal(&app(
            Some("firefox"),
            "Firefox",
            Some("https://kitty.example/")
        )));
        // Not part of excluded_apps: resetting that list keeps terminals refused.
        assert!(
            !default_excluded()
                .iter()
                .any(|e| TERMINALS.iter().any(|t| t.eq_ignore_ascii_case(e)))
        );
        let s: DoubleTapSettings = serde_json::from_str(r#"{"excluded_apps":[]}"#).unwrap();
        assert!(!s.allow_terminals);
    }

    #[test]
    fn provider_matching() {
        let providers: BTreeMap<String, String> = [
            ("com.tinyspeck.slackmacgap", "local"),
            ("google.com", "cloud-a"),
            ("mail.google.com", "cloud-b"),
            ("outlook", "cloud-c"),
            ("notes", "local-2"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let p = |a: AppInfo| provider_for(&a, &providers);
        assert_eq!(
            p(app(Some("com.tinyspeck.slackmacgap"), "Slack", None)).as_deref(),
            Some("local")
        );
        assert_eq!(
            p(app(Some("OUTLOOK.EXE"), "Outlook", None)).as_deref(),
            Some("cloud-c")
        );
        assert_eq!(
            p(app(
                Some("firefox"),
                "Firefox",
                Some("https://mail.google.com/u/0")
            ))
            .as_deref(),
            Some("cloud-b")
        );
        assert_eq!(
            p(app(
                Some("firefox"),
                "Firefox",
                Some("https://docs.google.com/")
            ))
            .as_deref(),
            Some("cloud-a")
        );
        assert_eq!(p(app(None, "Notes", None)).as_deref(), Some("local-2"));
        assert_eq!(
            p(app(
                Some("firefox"),
                "Firefox",
                Some("https://notgoogle.com/")
            )),
            None
        );
    }

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir = std::env::temp_dir().join(format!("nugget-doubletap-{}", std::process::id()));
        let path = file_in(&dir);
        assert_eq!(load(&path), DoubleTapSettings::default());
        let s = DoubleTapSettings {
            enabled: false,
            excluded_apps: vec!["Foo".into()],
            ..DoubleTapSettings::default()
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        // An old file without `read_ahead` loads with the default; turning it off persists.
        std::fs::write(&path, br#"{"enabled":true,"allow_terminals":false}"#).unwrap();
        assert!(load(&path).read_ahead);
        let off = DoubleTapSettings {
            read_ahead: false,
            ..DoubleTapSettings::default()
        };
        save(&path, &off).unwrap();
        assert!(!load(&path).read_ahead);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), DoubleTapSettings::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
