//! Apps whose surroundings are never read: password managers, banking and system settings by
//! default, plus whatever the user adds. In a denied app `focused()` returns no thread, no
//! recipients or subject and no field value, and `insert()` refuses.

use nugget_core::ipc::AppInfo;

/// One deny rule. Matching is case-insensitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyRule {
    /// App id (bundle id, executable name, desktop-file id). A trailing `*` matches a prefix.
    AppId(String),
    /// The app name equals this.
    NameIs(String),
    /// The app name contains this.
    NameContains(String),
    /// The page host is this domain or a subdomain of it.
    Host(String),
    /// The page host contains this.
    HostContains(String),
}

impl DenyRule {
    /// Parse a settings string: `id:com.example.app`, `id:com.example.*`, `name:Wallet`,
    /// `name~bank`, `host:example.com`, `host~bank`. A bare string is `name~<string>`.
    pub fn parse(s: &str) -> Option<DenyRule> {
        let s = s.trim();
        let (kind, val) = match s.find([':', '~']) {
            Some(i) => (&s[..=i], s[i + 1..].trim()),
            None => ("name~", s),
        };
        if val.is_empty() {
            return None;
        }
        let v = val.to_lowercase();
        Some(match kind {
            "id:" | "id~" => DenyRule::AppId(v),
            "name:" => DenyRule::NameIs(v),
            "name~" => DenyRule::NameContains(v),
            "host:" => DenyRule::Host(v.trim_start_matches('.').to_string()),
            "host~" => DenyRule::HostContains(v),
            _ => DenyRule::NameContains(s.to_lowercase()),
        })
    }

    pub fn matches(&self, app: &AppInfo) -> bool {
        let id = app.id.as_deref().unwrap_or("").to_lowercase();
        let name = app.name.trim().to_lowercase();
        let host = app.url.as_deref().and_then(host_of);
        match self {
            DenyRule::AppId(p) => match p.strip_suffix('*') {
                Some(prefix) => !id.is_empty() && id.starts_with(prefix),
                None => !id.is_empty() && (id == *p || id.trim_end_matches(".desktop") == *p),
            },
            DenyRule::NameIs(n) => name == *n,
            DenyRule::NameContains(n) => !n.is_empty() && name.contains(n.as_str()),
            DenyRule::Host(d) => host.is_some_and(|h| h == *d || h.ends_with(&format!(".{d}"))),
            DenyRule::HostContains(d) => host.is_some_and(|h| h.contains(d.as_str())),
        }
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next()?.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// A configurable deny list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyList {
    pub rules: Vec<DenyRule>,
}

impl Default for DenyList {
    fn default() -> Self {
        Self::defaults()
    }
}

const DEFAULT_RULES: &[&str] = &[
    // Password managers and keychains.
    "id:com.agilebits.*",
    "id:com.1password.*",
    "id:com.bitwarden.*",
    "id:com.lastpass.*",
    "id:com.dashlane.*",
    "id:org.keepassxc.*",
    "id:com.apple.keychainaccess",
    "id:com.apple.passwords",
    "id:org.gnome.seahorse*",
    "id:seahorse",
    "id:keepassxc*",
    "id:keepass*",
    "name~1password",
    "name~bitwarden",
    "name~lastpass",
    "name~dashlane",
    "name~keepass",
    "name~enpass",
    "name~nordpass",
    "name~proton pass",
    "name~roboform",
    "name~keychain access",
    "name:keeper",
    "name:keeper password manager",
    "name:passwords",
    "name:passwords and keys",
    "name:kwalletmanager",
    "name:kwallet manager",
    "host:1password.com",
    "host:bitwarden.com",
    "host:vault.bitwarden.com",
    "host:lastpass.com",
    "host:dashlane.com",
    "host:passwords.google.com",
    // System settings.
    "id:com.apple.systempreferences",
    "id:com.apple.settings*",
    "id:systemsettings.exe",
    "id:control.exe",
    "id:gnome-control-center",
    "id:org.gnome.settings",
    "id:systemsettings",
    "id:org.kde.systemsettings",
    "name:system settings",
    "name:system preferences",
    "name:settings",
    "name:control panel",
    "name:gnome-control-center",
    "name:systemsettings",
    // Banking, payments and brokerage (apps and sites).
    "name~bank",
    "name:chase",
    "name:chase mobile",
    "name:wells fargo",
    "name:revolut",
    "name:monzo",
    "name:n26",
    "name:starling",
    "name:paypal",
    "name:venmo",
    "name:wise",
    "name:cash app",
    "name:schwab",
    "name:fidelity",
    "name:vanguard",
    "name:robinhood",
    "name:coinbase",
    "host~bank",
    "host:chase.com",
    "host:wellsfargo.com",
    "host:citi.com",
    "host:capitalone.com",
    "host:americanexpress.com",
    "host:paypal.com",
    "host:venmo.com",
    "host:revolut.com",
    "host:monzo.com",
    "host:n26.com",
    "host:wise.com",
    "host:schwab.com",
    "host:fidelity.com",
    "host:vanguard.com",
    "host:robinhood.com",
    "host:coinbase.com",
    "host:barclays.co.uk",
    "host:hsbc.com",
    "host:santander.com",
    "host:lloydsbank.com",
];

impl DenyList {
    /// The shipped defaults: password managers, banking and system settings.
    pub fn defaults() -> Self {
        DenyList {
            rules: DEFAULT_RULES
                .iter()
                .filter_map(|r| DenyRule::parse(r))
                .collect(),
        }
    }

    pub fn empty() -> Self {
        DenyList { rules: Vec::new() }
    }

    /// Defaults plus user rules in settings syntax (see [`DenyRule::parse`]).
    pub fn with_user_rules<I, S>(user: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut list = Self::defaults();
        list.rules
            .extend(user.into_iter().filter_map(|s| DenyRule::parse(s.as_ref())));
        list
    }

    pub fn push(&mut self, rule: DenyRule) {
        self.rules.push(rule);
    }

    pub fn is_denied(&self, app: &AppInfo) -> bool {
        self.rules.iter().any(|r| r.matches(app))
    }
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
    fn defaults_cover_password_managers_banking_and_settings() {
        let d = DenyList::defaults();
        assert!(d.is_denied(&app(
            Some("com.agilebits.onepassword7"),
            "1Password 7",
            None
        )));
        assert!(d.is_denied(&app(Some("com.bitwarden.desktop"), "Bitwarden", None)));
        assert!(d.is_denied(&app(Some("org.keepassxc.KeePassXC"), "KeePassXC", None)));
        assert!(d.is_denied(&app(
            Some("com.apple.systempreferences"),
            "System Settings",
            None
        )));
        assert!(d.is_denied(&app(Some("SystemSettings.exe"), "Settings", None)));
        assert!(d.is_denied(&app(Some("gnome-control-center.desktop"), "Settings", None)));
        assert!(d.is_denied(&app(None, "Bank of Example", None)));
        assert!(d.is_denied(&app(
            Some("com.google.Chrome"),
            "Google Chrome",
            Some("https://secure.chase.com/web/auth")
        )));
        assert!(d.is_denied(&app(
            Some("firefox"),
            "Firefox",
            Some("https://online.examplebank.co.uk/login")
        )));
        assert!(!d.is_denied(&app(Some("com.tinyspeck.slackmacgap"), "Slack", None)));
        assert!(!d.is_denied(&app(
            Some("com.google.Chrome"),
            "Google Chrome",
            Some("https://mail.google.com/")
        )));
        // "settings" as a whole name only; "wise" not as a substring.
        assert!(!d.is_denied(&app(None, "Settings Sync Notes", None)));
        assert!(!d.is_denied(&app(None, "Otherwise", None)));
        // A host that merely mentions a bank domain in the path is not denied by host rules.
        assert!(!d.is_denied(&app(
            Some("firefox"),
            "Firefox",
            Some("https://news.example/chase.com")
        )));
    }

    #[test]
    fn user_rules_parse() {
        assert_eq!(
            DenyRule::parse("id:com.example.*"),
            Some(DenyRule::AppId("com.example.*".into()))
        );
        assert_eq!(
            DenyRule::parse("host:.Example.com"),
            Some(DenyRule::Host("example.com".into()))
        );
        assert_eq!(
            DenyRule::parse("host~corp"),
            Some(DenyRule::HostContains("corp".into()))
        );
        assert_eq!(
            DenyRule::parse("Wallet"),
            Some(DenyRule::NameContains("wallet".into()))
        );
        assert_eq!(DenyRule::parse("name:"), None);
        let d = DenyList::with_user_rules(["name:Signal", "host:intranet.corp"]);
        assert!(d.is_denied(&app(None, "Signal", None)));
        assert!(d.is_denied(&app(None, "Edge", Some("http://wiki.intranet.corp:8080/x"))));
        assert!(DenyList::empty().rules.is_empty());
    }
}
