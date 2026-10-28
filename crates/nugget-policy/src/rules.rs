//! Rules per app and site, the built-in defaults, and the persisted [`PolicyConfig`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::glob::{glob_match, specificity};
use crate::{ActionContext, ActionDescriptor, RiskClass};

/// What a matching rule does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Allow,
    Confirm,
    Deny,
}

/// One rule. Every non-empty field must match; an empty field matches anything.
///
/// - `targets`: globs over the action's target candidates (the app id, hosts of URLs and deep
///   links found in the action and its context, deep-link schemes and panes). See
///   [`target_candidates`].
/// - `verbs`: globs over the executor verb.
/// - `risks`: effective risk classes (after the classifier). An `Allow` rule with no `risks`
///   only covers Observe and Edit.
/// - `labels`: globs over the target label and the window title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable id, shown in explanations and the log (`default:*` ids are reserved).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verbs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<RiskClass>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    pub effect: Effect,
}

impl Rule {
    pub fn new(id: impl Into<String>, effect: Effect) -> Self {
        Self {
            id: id.into(),
            description: None,
            targets: Vec::new(),
            verbs: Vec::new(),
            risks: Vec::new(),
            labels: Vec::new(),
            effect,
        }
    }
    pub fn describe(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }
    pub fn targets<I: IntoIterator<Item = S>, S: Into<String>>(mut self, globs: I) -> Self {
        self.targets = globs.into_iter().map(Into::into).collect();
        self
    }
    pub fn verbs<I: IntoIterator<Item = S>, S: Into<String>>(mut self, globs: I) -> Self {
        self.verbs = globs.into_iter().map(Into::into).collect();
        self
    }
    pub fn risks<I: IntoIterator<Item = RiskClass>>(mut self, risks: I) -> Self {
        self.risks = risks.into_iter().collect();
        self
    }
    pub fn labels<I: IntoIterator<Item = S>, S: Into<String>>(mut self, globs: I) -> Self {
        self.labels = globs.into_iter().map(Into::into).collect();
        self
    }

    /// If the rule matches, how specific the match was (higher wins among Allow/Confirm).
    pub(crate) fn matches(&self, m: &MatchInput<'_>) -> Option<usize> {
        let mut score = 0;
        if !self.targets.is_empty() {
            let best = self
                .targets
                .iter()
                .filter(|g| m.targets.iter().any(|t| glob_match(g, t)))
                .map(|g| specificity(g))
                .max()?;
            score += 1000 + best;
        }
        if !self.verbs.is_empty() {
            if !self.verbs.iter().any(|g| glob_match(g, m.verb)) {
                return None;
            }
            score += 100;
        }
        if self.risks.is_empty() {
            if self.effect == Effect::Allow && m.risk > RiskClass::Edit {
                return None;
            }
        } else {
            if !self.risks.contains(&m.risk) {
                return None;
            }
            score += 50;
        }
        if !self.labels.is_empty() {
            if !self
                .labels
                .iter()
                .any(|g| m.labels.iter().any(|l| glob_match(g, l)))
            {
                return None;
            }
            score += 100;
        }
        Some(score)
    }
}

/// What rules are matched against.
pub(crate) struct MatchInput<'a> {
    pub targets: &'a [String],
    pub verb: &'a str,
    pub risk: RiskClass,
    pub labels: &'a [String],
}

/// The persisted, user-editable part of the policy. Built-in defaults ([`default_rules`]) are
/// not stored, so they improve with the app; `disabled_defaults` opts out of one by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyConfig {
    #[serde(default = "PolicyConfig::current_version")]
    pub version: u32,
    /// User rules. They beat default Allow/Confirm rules; any Deny wins over everything.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Ids of default rules the user turned off.
    #[serde(default)]
    pub disabled_defaults: Vec<String>,
    /// Maximum number of actions per task in each risk class (Allow plus approved Confirm).
    #[serde(default = "PolicyConfig::default_rate_limits")]
    pub rate_limits: BTreeMap<RiskClass, u32>,
    /// How long a confirmation waits for the user before it counts as deny.
    #[serde(default = "PolicyConfig::default_confirm_timeout_secs")]
    pub confirm_timeout_secs: u64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            version: Self::current_version(),
            rules: Vec::new(),
            disabled_defaults: Vec::new(),
            rate_limits: Self::default_rate_limits(),
            confirm_timeout_secs: Self::default_confirm_timeout_secs(),
        }
    }
}

impl PolicyConfig {
    fn current_version() -> u32 {
        1
    }
    fn default_confirm_timeout_secs() -> u64 {
        120
    }
    /// Sends 25, deletions 25, spends 3, security changes 3 per task. Observe and Edit are
    /// unlimited.
    pub fn default_rate_limits() -> BTreeMap<RiskClass, u32> {
        BTreeMap::from([
            (RiskClass::Send, 25),
            (RiskClass::Destroy, 25),
            (RiskClass::Spend, 3),
            (RiskClass::Security, 3),
        ])
    }

    /// The default rules still enabled, followed by nothing else (user rules are separate).
    pub fn active_defaults(&self) -> Vec<Rule> {
        default_rules()
            .into_iter()
            .filter(|r| !self.disabled_defaults.contains(&r.id))
            .collect()
    }
}

/// Every target the rules should see for this action: the app id, the hosts of URLs and deep
/// links in the action and context, deep-link schemes and settings panes. All lowercase.
pub fn target_candidates(action: &ActionDescriptor, ctx: &ActionContext) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        let s = s.trim().to_lowercase();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    let add_link = |text: &str, push: &mut dyn FnMut(String)| {
        let text = text.trim();
        push(text.to_string());
        if let Some(host) = host_of(text) {
            push(host.clone());
            if let Some(stripped) = host.strip_prefix("www.") {
                push(stripped.to_string());
            }
        }
        if let Some((scheme, rest)) = text.split_once(':')
            && is_scheme(scheme)
        {
            push(scheme.to_string());
            // `x-apple.systempreferences:com.apple.preference.security?Privacy`
            let pane = rest
                .trim_start_matches('/')
                .split(['?', '#', '/', '&'])
                .next()
                .unwrap_or("");
            if !pane.is_empty() {
                push(pane.to_string());
            }
        }
    };
    add_link(&action.target_app, &mut push);
    if let Some(url) = &ctx.url {
        add_link(url, &mut push);
    }
    for text in [
        action.target_label.as_deref(),
        action.text_preview.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        for token in text.split(char::is_whitespace) {
            let token =
                token.trim_matches(|c: char| matches!(c, '"' | '\'' | '<' | '>' | '(' | ')'));
            if token.contains("://") || is_deep_link(token) {
                add_link(token, &mut push);
            } else if is_bare_domain(token) && navigates(action) {
                add_link(&format!("https://{token}"), &mut push);
            }
        }
    }
    out
}

/// Host of `scheme://host/...`; `None` for anything else.
pub(crate) fn host_of(text: &str) -> Option<String> {
    let (scheme, rest) = text.trim().split_once("://")?;
    if !is_scheme(scheme) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?.trim_end_matches('.').to_lowercase();
    (!host.is_empty()).then_some(host)
}

fn is_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `scheme:rest` where the scheme is not a drive letter or a time ("10:30").
fn is_deep_link(token: &str) -> bool {
    token.split_once(':').is_some_and(|(scheme, rest)| {
        scheme.len() > 1 && is_scheme(scheme) && !rest.is_empty() && !rest.starts_with(' ')
    })
}

fn is_bare_domain(token: &str) -> bool {
    let token = token.trim_end_matches(['/', '.', ',']);
    let host = token.split('/').next().unwrap_or("");
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && parts
            .last()
            .is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Typing a bare domain only navigates in an address bar or with a navigation verb.
fn navigates(action: &ActionDescriptor) -> bool {
    let verb = action.verb.to_lowercase();
    let label = action.target_label.as_deref().unwrap_or("").to_lowercase();
    [
        "open", "navigate", "goto", "go_to", "visit", "launch", "load", "browse",
    ]
    .iter()
    .any(|v| verb.contains(v))
        || ["address", "url", "location", "search or", "omnibox"]
            .iter()
            .any(|l| label.contains(l))
}

// ---------------------------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------------------------

/// The built-in rules, in this order:
/// 1. `default:deny-password-managers`: everything in password managers and OS keychains.
/// 2. `default:deny-financial`: everything in banking, brokerage, crypto-exchange and payment
///    wallet apps and sites.
/// 3. `default:deny-os-security`: OS security and privacy settings, security tools and their
///    deep links.
/// 4. `default:deny-security-panes`: security, privacy, password and user panes inside general
///    settings apps (matched on the label or window title).
/// 5. `default:confirm-risky`: Send, Destroy, Spend and Security everywhere.
/// 6. `default:allow-observe-edit`: Observe and Edit everywhere.
pub fn default_rules() -> Vec<Rule> {
    let financial: Vec<String> = FINANCIAL_BRANDS
        .iter()
        .flat_map(|b| {
            [
                b.to_string(),
                format!("{b}.*"),
                format!("*.{b}.*"),
                format!("*.{b}"),
            ]
        })
        .chain(FINANCIAL_EXACT.iter().map(|s| s.to_string()))
        .collect();
    vec![
        Rule::new("default:deny-password-managers", Effect::Deny)
            .describe("password managers and keychains")
            .targets(PASSWORD_MANAGERS.iter().copied()),
        Rule::new("default:deny-financial", Effect::Deny)
            .describe("banking, brokerage and payment apps and sites")
            .targets(financial),
        Rule::new("default:deny-os-security", Effect::Deny)
            .describe("OS security and privacy settings")
            .targets(OS_SECURITY.iter().copied()),
        Rule::new("default:deny-security-panes", Effect::Deny)
            .describe("security, privacy and account panes of system settings")
            .targets(SETTINGS_APPS.iter().copied())
            .labels(SECURITY_PANE_LABELS.iter().copied()),
        Rule::new("default:confirm-risky", Effect::Confirm)
            .describe("sending, deleting, spending and security changes")
            .risks([
                RiskClass::Send,
                RiskClass::Destroy,
                RiskClass::Spend,
                RiskClass::Security,
            ]),
        Rule::new("default:allow-observe-edit", Effect::Allow)
            .describe("reading and undoable edits")
            .risks([RiskClass::Observe, RiskClass::Edit]),
    ]
}

const PASSWORD_MANAGERS: &[&str] = &[
    "*1password*",
    "onepassword*",
    "com.agilebits.*",
    "op",
    "*bitwarden*",
    "*lastpass*",
    "*dashlane*",
    "*keepass*",
    "*enpass*",
    "*nordpass*",
    "*keepersecurity*",
    "com.callpod.*",
    "*roboform*",
    "*protonpass*",
    "pass.proton.me",
    "me.proton.pass*",
    "*passwordmanager*",
    "*password-manager*",
    "com.apple.keychainaccess",
    "com.apple.passwords*",
    "com.apple.keychain*",
    "passwords.google.com",
    "passwords.google",
    "chrome://password-manager*",
    "chrome://settings/passwords*",
    "edge://wallet*",
    "edge://settings/passwords*",
    "about:logins*",
    "*seahorse*",
    "*kwalletmanager*",
    "credwiz*",
    "*credentialmanager*",
    "*credential manager*",
    "keymgr*",
];

/// Brands expanded to `b`, `b.*`, `*.b.*` and `*.b` (app ids, deep-link schemes, hosts).
const FINANCIAL_BRANDS: &[&str] = &[
    "chase",
    "wellsfargo",
    "bankofamerica",
    "citi",
    "citibank",
    "capitalone",
    "hsbc",
    "barclays",
    "santander",
    "lloydsbank",
    "natwest",
    "rbcroyalbank",
    "tdbank",
    "scotiabank",
    "bmo",
    "bnpparibas",
    "societegenerale",
    "credit-agricole",
    "commerzbank",
    "sparkasse",
    "unicredit",
    "intesasanpaolo",
    "bbva",
    "caixabank",
    "nordea",
    "revolut",
    "monzo",
    "n26",
    "starlingbank",
    "ally",
    "usaa",
    "schwab",
    "fidelity",
    "vanguard",
    "etrade",
    "robinhood",
    "interactivebrokers",
    "ibkr",
    "webull",
    "tdameritrade",
    "merrilledge",
    "sofi",
    "wealthfront",
    "betterment",
    "coinbase",
    "binance",
    "etoro",
    "trading212",
    "degiro",
    "wealthsimple",
    "questrade",
    "zerodha",
    "groww",
    "upstox",
    "paypal",
    "venmo",
    "zellepay",
    "americanexpress",
    "amex",
    "alipay",
    "paytm",
    "phonepe",
    "mercadopago",
    "nubank",
    "klarna",
    "transferwise",
    "westernunion",
    "moneygram",
];

/// Exact patterns for brands whose names collide with unrelated hosts.
const FINANCIAL_EXACT: &[&str] = &[
    "*bank*",
    "*banking*",
    "*banque*",
    "*banco*",
    "*brokerage*",
    "kraken.com",
    "*.kraken.com",
    "com.kraken.*",
    "gemini.com",
    "exchange.gemini.com",
    "com.gemini.android*",
    "crypto.com",
    "*.crypto.com",
    "wise.com",
    "*.wise.com",
    "com.transferwise.*",
    "cash.app",
    "*.cash.app",
    "com.squareup.cash*",
    "cashme",
    "ing.com",
    "*.ing.com",
    "ing.*",
    "com.ing.*",
    "discover.com",
    "*.discover.com",
    "com.discoverfinancial.*",
    "td.com",
    "*.td.com",
    "com.td.*",
    "chime.com",
    "*.chime.com",
    "com.onedebit.chime*",
    "pay.google.com",
    "wallet.google.com",
    "wallet.apple.com",
    "com.apple.passbook",
    "shoebox",
];

const OS_SECURITY: &[&str] = &[
    // macOS panes and deep links
    "com.apple.preference.security*",
    "com.apple.settings.privacysecurity*",
    "com.apple.*privacy*",
    "com.apple.*security*",
    "com.apple.*password*",
    "com.apple.*users-groups*",
    "com.apple.preferences.users*",
    "com.apple.*loginitems*",
    "com.apple.*touchid*",
    "com.apple.*filevault*",
    "com.apple.*profiles*",
    // Windows
    "ms-settings:windowsdefender*",
    "ms-settings:privacy*",
    "ms-settings:signinoptions*",
    "ms-settings:otherusers*",
    "ms-settings:family-group*",
    "ms-settings:developers*",
    "ms-settings:workplace*",
    "ms-settings:emailandaccounts*",
    "windowsdefender*",
    "*sechealthui*",
    "*securityhealth*",
    "*windowssecurity*",
    "*windows security*",
    "secpol.msc",
    "gpedit.msc",
    "regedit*",
    "useraccountcontrolsettings*",
    "lusrmgr.msc",
    "certmgr.msc",
    "certlm.msc",
    "wf.msc",
    "firewall.cpl",
    "netplwiz*",
    "bitlocker*",
    "*manage-bde*",
    // Linux
    "*gufw*",
    "*firewall-config*",
    "*firewalld*",
    "*polkit*",
    "gnome-control-center privacy*",
    "org.gnome.settings.privacy*",
    "*usermod*",
    "*passwd*",
    // Browsers
    "chrome://settings/security*",
    "chrome://settings/privacy*",
    "edge://settings/privacy*",
    "about:preferences#privacy*",
];

const SETTINGS_APPS: &[&str] = &[
    "com.apple.systempreferences",
    "com.apple.settings*",
    "x-apple.systempreferences",
    "system settings",
    "system preferences",
    "ms-settings*",
    "windows.immersivecontrolpanel*",
    "systemsettings*",
    "control.exe",
    "*control panel*",
    "gnome-control-center*",
    "org.gnome.settings*",
    "org.kde.systemsettings*",
    "com.android.settings",
    "chrome://settings*",
    "edge://settings*",
    "about:preferences*",
];

const SECURITY_PANE_LABELS: &[&str] = &[
    "*security*",
    "*privacy*",
    "*password*",
    "*passkey*",
    "*firewall*",
    "*filevault*",
    "*gatekeeper*",
    "*touch id*",
    "*face id*",
    "*login item*",
    "*users & groups*",
    "*users and groups*",
    "*full disk access*",
    "*screen recording*",
    "*accessibility*",
    "*defender*",
    "*bitlocker*",
    "*encryption*",
    "*sign-in options*",
    "*user account control*",
    "*developer mode*",
    "*lock screen*",
    "*keychain*",
    "*sicherheit*",
    "*datenschutz*",
    "*seguridad*",
    "*privacidad*",
    "*sécurité*",
    "*confidentialité*",
    "*sicurezza*",
    "*segurança*",
    "*beveiliging*",
    "*безопасност*",
    "*конфиденциальн*",
    "*安全*",
    "*隐私*",
    "*隱私*",
    "*セキュリティ*",
    "*プライバシー*",
    "*보안*",
    "*개인정보*",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn action(
        app: &str,
        verb: &str,
        label: Option<&str>,
        preview: Option<&str>,
    ) -> ActionDescriptor {
        ActionDescriptor {
            target_app: app.into(),
            verb: verb.into(),
            target_label: label.map(Into::into),
            text_preview: preview.map(Into::into),
            declared_risk: RiskClass::Observe,
            task_id: "t".into(),
        }
    }

    #[test]
    fn hosts_and_schemes() {
        assert_eq!(
            host_of("https://user@Secure.Chase.com:443/x?y").as_deref(),
            Some("secure.chase.com")
        );
        assert_eq!(host_of("chase.com"), None);
        let c = target_candidates(
            &action(
                "com.apple.Safari",
                "open_url",
                Some("onepassword://open?item=1"),
                None,
            ),
            &ActionContext::default(),
        );
        assert!(c.contains(&"com.apple.safari".to_string()));
        assert!(c.contains(&"onepassword".to_string()));
        let c = target_candidates(
            &action(
                "x",
                "open_url",
                Some("x-apple.systempreferences:com.apple.preference.security?Privacy"),
                None,
            ),
            &ActionContext::default(),
        );
        assert!(
            c.contains(&"com.apple.preference.security".to_string()),
            "{c:?}"
        );
    }

    #[test]
    fn bare_domains_only_when_navigating() {
        let typed = action(
            "com.google.chrome",
            "type",
            Some("Address and search bar"),
            Some("robinhood.com"),
        );
        assert!(
            target_candidates(&typed, &ActionContext::default())
                .contains(&"robinhood.com".to_string())
        );
        let email = action(
            "com.apple.mail",
            "type",
            Some("Message body"),
            Some("my bank is chase.com"),
        );
        assert!(
            !target_candidates(&email, &ActionContext::default())
                .contains(&"chase.com".to_string())
        );
        assert!(!is_bare_domain("10.30"));
        assert!(!is_deep_link("10:30"));
    }

    #[test]
    fn page_url_is_a_candidate() {
        let a = action("com.google.chrome", "click", Some("View"), None);
        let ctx = ActionContext::default().with_url("https://www.bankofamerica.com/accounts");
        let c = target_candidates(&a, &ctx);
        assert!(c.contains(&"bankofamerica.com".to_string()));
    }

    #[test]
    fn allow_without_risks_covers_only_observe_and_edit() {
        let rule = Rule::new("u", Effect::Allow).targets(["com.apple.mail"]);
        let targets = vec!["com.apple.mail".to_string()];
        let input = |risk| MatchInput {
            targets: &targets,
            verb: "click",
            risk,
            labels: &[],
        };
        assert!(rule.matches(&input(RiskClass::Edit)).is_some());
        assert!(rule.matches(&input(RiskClass::Send)).is_none());
        let explicit = rule.clone().risks([RiskClass::Send]);
        assert!(explicit.matches(&input(RiskClass::Send)).is_some());
    }

    #[test]
    fn config_serde_round_trip() {
        let mut cfg = PolicyConfig::default();
        cfg.rules.push(
            Rule::new("u1", Effect::Confirm)
                .targets(["*.example.com"])
                .verbs(["click"])
                .risks([RiskClass::Edit])
                .labels(["*save*"]),
        );
        cfg.disabled_defaults
            .push("default:allow-observe-edit".into());
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        assert!(json.contains("\"send\": 25"), "{json}");
        let back: PolicyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
        let minimal: PolicyConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(minimal, PolicyConfig::default());
    }
}
