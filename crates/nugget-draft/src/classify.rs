//! Rule-based field classification and the pre-draft gate.
//!
//! The kind of a field is decided here from what the accessibility layer read (role, app id,
//! page URL, label, placeholder) and the conversation around it, never by asking the model. Rules
//! run in a fixed order, strongest signal first:
//!
//! 1. **Search**: a search role, a search-like label or placeholder, or a launcher app.
//! 2. **Explicit labels**: "Message body" (email), "Add a comment" (comment), "Message #general"
//!    or "Type a message" (chat), "Add a description" (document).
//! 3. **App family**, from the app id (bundle id, executable or desktop-file id) or the page host
//!    in a browser: mail clients, chat apps, comment/issue platforms, document editors. Header
//!    fields of a family (subject, to, title) are form fields.
//! 4. **Conversation**: a subject or recipients mean email; a thread means chat.
//! 5. **Role**: a single-line field is a form field, a document role is a document.
//!
//! [`gate`] runs first and refuses secure fields (password, one-time code, card number),
//! excluded apps (password managers by default) and terminal emulators ([`DEFAULT_TERMINAL_APPS`],
//! unless allowed with [`gate_with`]) before anything is classified or read further.

use nugget_core::ipc::DraftContext;

use crate::FieldKind;

/// Why a draft was refused before classification or generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A password or otherwise protected field, flagged by the OS or recognised by its label.
    SecureField,
    /// The app is on the excluded list (password managers by default).
    ExcludedApp(String),
    /// The field kind makes no sense to draft into (a search box).
    NotDraftable(FieldKind),
    /// A terminal emulator ([`DEFAULT_TERMINAL_APPS`]): inserted text could run as a command.
    /// Allowed only with `DraftConfig::allow_terminals`.
    Terminal(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::SecureField => {
                f.write_str("drafting is refused in password and secure fields")
            }
            Refusal::ExcludedApp(app) => write!(f, "drafting is disabled in {app}"),
            Refusal::NotDraftable(kind) => {
                write!(f, "nothing to draft in a {} field", kind.as_str())
            }
            Refusal::Terminal(app) => {
                write!(f, "drafting is disabled in terminals ({app})")
            }
        }
    }
}

impl From<Refusal> for nugget_core::Error {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::SecureField | Refusal::ExcludedApp(_) | Refusal::Terminal(_) => {
                nugget_core::Error::Invalid(r.to_string())
            }
            Refusal::NotDraftable(_) => nugget_core::Error::Unsupported(r.to_string()),
        }
    }
}

/// Password managers and system credential stores: never drafted into, whatever the field.
pub const DEFAULT_EXCLUDED_APPS: &[&str] = &[
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.agilebits.onepassword-osx",
    "1password",
    "com.bitwarden.desktop",
    "bitwarden",
    "org.keepassxc.keepassxc",
    "keepassxc",
    "keepass",
    "com.lastpass.lastpass",
    "com.dashlane.dashlane",
    "com.apple.keychainaccess",
    "com.apple.passwords",
    "org.gnome.seahorse.application",
    // Proton Pass.
    "me.proton.pass.electron",
    "me.proton.pass",
    "protonpass",
    "proton pass",
    "proton-pass",
    "pass.proton.me",
    // Enpass.
    "in.sinew.enpass-desktop",
    "in.sinew.enpass",
    "enpass",
    // NordPass.
    "com.nordsec.nordpass",
    "nordpass",
    "nordpass.com",
    // Keeper and RoboForm.
    "com.keepersecurity.passwordmanager",
    "keeperpasswordmanager",
    "keeper password manager",
    "keepersecurity.com",
    "com.siber.roboform",
    "roboform",
    "roboform.com",
    "my.1password.com",
    "vault.bitwarden.com",
    "lastpass.com",
];

/// Terminal emulators: refused by default (text typed into a shell can run as a command),
/// unless `DraftConfig::allow_terminals`. App ids are matched like excluded apps (bundle ids,
/// executables without `.exe`, desktop-file ids, all case-insensitive).
pub const DEFAULT_TERMINAL_APPS: &[&str] = &[
    // macOS.
    "com.apple.terminal",
    "com.googlecode.iterm2",
    "com.mitchellh.ghostty",
    "com.github.wez.wezterm",
    "org.alacritty",
    "io.alacritty",
    "net.kovidgoyal.kitty",
    // Windows.
    "windowsterminal",
    "openconsole",
    "conhost",
    "cmd",
    "powershell",
    "powershell_ise",
    "pwsh",
    // Linux.
    "org.gnome.terminal",
    "gnome-terminal",
    "gnome-terminal-server",
    "org.gnome.console",
    "kgx",
    "org.kde.konsole",
    "konsole",
    "alacritty",
    "kitty",
    "wezterm",
    "wezterm-gui",
    "org.wezfurlong.wezterm",
    "ghostty",
    "xterm",
    "uxterm",
    "dev.warp.warp-stable",
    "dev.warp.warp",
    "warp",
    "com.gexperts.tilix",
    "tilix",
    "org.gnome.ptyxis",
    "ptyxis",
    "terminator",
    "foot",
    "footclient",
    "rxvt",
    "urxvt",
    "co.zeit.hyper",
    "hyper",
    "org.tabby",
    "tabby",
];

/// Whether the focused app is a terminal emulator ([`DEFAULT_TERMINAL_APPS`]).
/// Also any app whose id contains `terminal` (`xfce4-terminal`, `mate-terminal`), or whose name
/// has the word "Terminal".
pub fn is_terminal(ctx: &DraftContext) -> bool {
    let s = Signals::of(ctx);
    (!s.app_id.is_empty()
        && (DEFAULT_TERMINAL_APPS
            .iter()
            .any(|t| s.app_id == *t || s.app_id_raw == *t)
            || s.app_id.contains("terminal")))
        || words(&s.app_name).any(|w| w == "terminal")
}

/// Refuse secure fields, excluded apps and terminals, then classify. `excluded` holds app ids
/// (matched after normalisation, see [`normalize_app_id`]) and page hosts (matched with
/// subdomains). Same as [`gate_with`] with terminals refused.
pub fn gate(ctx: &DraftContext, excluded: &[String]) -> Result<FieldKind, Refusal> {
    gate_with(ctx, excluded, false)
}

/// [`gate`], with terminal emulators allowed when `allow_terminals`.
pub fn gate_with(
    ctx: &DraftContext,
    excluded: &[String],
    allow_terminals: bool,
) -> Result<FieldKind, Refusal> {
    if is_secure(ctx) {
        return Err(Refusal::SecureField);
    }
    let s = Signals::of(ctx);
    for e in excluded {
        let host = e.trim().to_ascii_lowercase();
        if host.is_empty() {
            continue;
        }
        if s.app_id == normalize_app_id(e) || s.app_id_raw == host || host_is(&s.host, &host) {
            return Err(Refusal::ExcludedApp(display_app(ctx)));
        }
    }
    if !allow_terminals && is_terminal(ctx) {
        return Err(Refusal::Terminal(display_app(ctx)));
    }
    let kind = classify(ctx);
    if !kind.draftable() {
        return Err(Refusal::NotDraftable(kind));
    }
    Ok(kind)
}

fn display_app(ctx: &DraftContext) -> String {
    if !ctx.app.name.trim().is_empty() {
        ctx.app.name.trim().to_owned()
    } else {
        ctx.app.id.clone().unwrap_or_else(|| "this app".into())
    }
}

/// Whether the field is protected: flagged secure by the accessibility API, a secure role, or a
/// label/placeholder naming a credential (password, passcode, PIN, one-time or verification code,
/// card number, CVV, social security number).
pub fn is_secure(ctx: &DraftContext) -> bool {
    if ctx.field.secure {
        return true;
    }
    let role = ctx.field.role.to_ascii_lowercase();
    if role.contains("secure") || role.contains("password") {
        return true;
    }
    let text = field_text(ctx);
    const PHRASES: &[&str] = &[
        "password",
        "passwort",
        "passphrase",
        "passcode",
        "one-time code",
        "one time code",
        "verification code",
        "security code",
        "authentication code",
        "2fa code",
        "card number",
        "credit card",
        "social security",
        "recovery key",
        "secret key",
        "private key",
        "api key",
    ];
    const WORDS: &[&str] = &["pin", "otp", "cvv", "cvc", "ssn", "mfa", "totp", "iban"];
    PHRASES.iter().any(|p| text.contains(p)) || words(&text).any(|w| WORDS.contains(&w))
}

/// Lower-cased app id with platform suffixes removed: `Slack.exe` → `slack`,
/// `org.gnome.Evolution.desktop` → `org.gnome.evolution`.
pub fn normalize_app_id(id: &str) -> String {
    let id = id.trim().to_ascii_lowercase();
    let id = id.rsplit(['/', '\\']).next().unwrap_or(&id).to_owned();
    for suffix in [".exe", ".desktop", ".app"] {
        if let Some(stripped) = id.strip_suffix(suffix) {
            return stripped.to_owned();
        }
    }
    id
}

/// The lower-cased host and path of a URL (`https://user@Mail.Google.com:443/mail/u/0` →
/// `("mail.google.com", "/mail/u/0")`). Anything unparsable gives empty strings.
pub fn url_host_path(url: &str) -> (String, String) {
    let url = url.trim();
    let rest = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let path = &rest[end..];
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or("").to_owned()
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    (
        host.trim_end_matches('.').to_ascii_lowercase(),
        path.to_ascii_lowercase(),
    )
}

/// `host` is `domain` or one of its subdomains.
fn host_is(host: &str, domain: &str) -> bool {
    !host.is_empty()
        && (host == domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.')))
}

fn field_text(ctx: &DraftContext) -> String {
    let mut t = String::new();
    for s in [&ctx.field.label, &ctx.field.placeholder]
        .into_iter()
        .flatten()
    {
        t.push_str(s);
        t.push(' ');
    }
    t.to_lowercase()
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

/// Everything the rules look at, normalised once.
struct Signals {
    app_id: String,
    /// The app id lower-cased, without suffix removal.
    app_id_raw: String,
    app_name: String,
    host: String,
    path: String,
    role: String,
    /// Label and placeholder, lower-cased.
    text: String,
}

impl Signals {
    fn of(ctx: &DraftContext) -> Self {
        let (host, path) = ctx
            .app
            .url
            .as_deref()
            .map(url_host_path)
            .unwrap_or_default();
        Self {
            app_id: ctx
                .app
                .id
                .as_deref()
                .map(normalize_app_id)
                .unwrap_or_default(),
            app_id_raw: ctx
                .app
                .id
                .as_deref()
                .map(|id| id.trim().to_ascii_lowercase())
                .unwrap_or_default(),
            app_name: ctx.app.name.trim().to_lowercase(),
            host,
            path,
            role: ctx.field.role.trim().to_ascii_lowercase(),
            text: field_text(ctx).trim().to_owned(),
        }
    }

    fn single_line(&self) -> bool {
        matches!(self.role.as_str(), "text_field" | "combo_box")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Mail,
    Chat,
    Comments,
    Docs,
    Launcher,
}

/// App ids (normalised) per family.
const APP_IDS: &[(Family, &[&str])] = &[
    (
        Family::Mail,
        &[
            "com.apple.mail",
            "com.microsoft.outlook",
            "outlook",
            "olk",
            "org.mozilla.thunderbird",
            "thunderbird",
            "net.thunderbird.thunderbird",
            "org.gnome.evolution",
            "evolution",
            "org.gnome.geary",
            "geary",
            "com.readdle.smartemail-mac",
            "com.readdle.spark",
            "it.bloop.airmail2",
            "com.mimestream.mimestream",
            "com.superhuman.electron",
            "superhuman",
            "com.hey.app.desktop",
            "com.getmailspring.mailspring",
            "mailspring",
            "mailclient",
            "com.canary.mail",
            "io.canarymail.mac",
            "org.kde.kmail2",
            "kmail",
            "proton mail",
            "ch.protonmail.desktop",
        ],
    ),
    (
        Family::Chat,
        &[
            "com.tinyspeck.slackmacgap",
            "com.slack.slack",
            "slack",
            "com.microsoft.teams",
            "com.microsoft.teams2",
            "teams",
            "ms-teams",
            "msteams",
            "com.hnc.discord",
            "com.discordapp.discord",
            "discord",
            "ru.keepcoder.telegram",
            "org.telegram.desktop",
            "telegramdesktop",
            "telegram",
            "net.whatsapp.whatsapp",
            "whatsapp",
            "desktop.whatsapp",
            "org.whispersystems.signal-desktop",
            "org.signal.signal",
            "signal",
            "com.apple.mobilesms",
            "com.apple.ichat",
            "com.facebook.archon",
            "messenger",
            "im.riot.riot",
            "element",
            "com.mattermost.desktop",
            "mattermost",
            "org.zulip.zulip-electron",
            "zulip",
            "com.tencent.xinwechat",
            "wechat",
            "jp.naver.line.mac",
            "line",
            "com.skype.skype",
            "skype",
            "us.zoom.xos",
            "zoom",
            "com.google.chat",
            "com.rocket.chat",
            "rocket.chat",
            "beeper",
            "com.automattic.beeper.desktop",
        ],
    ),
    (
        Family::Comments,
        &[
            "com.linear",
            "linear",
            "com.atlassian.jira",
            "com.asana.app",
            "asana",
            "com.figma.desktop",
            "figma",
            "com.clickup.desktop-app",
        ],
    ),
    (
        Family::Docs,
        &[
            "com.microsoft.word",
            "winword",
            "com.apple.iwork.pages",
            "notion.id",
            "notion",
            "md.obsidian",
            "obsidian",
            "com.apple.notes",
            "net.shinyfrog.bear",
            "com.apple.textedit",
            "org.libreoffice.libreoffice",
            "libreoffice-writer",
            "soffice",
            "swriter",
            "notepad",
            "org.gnome.texteditor",
            "org.gnome.gedit",
            "gedit",
            "abnerworks.typora",
            "typora",
            "com.lukilabs.lukiapp",
            "com.microsoft.onenote.mac",
            "onenote",
            "com.evernote.evernote",
            "evernote",
            "com.ulyssesapp.mac",
            "com.coda.desktop",
        ],
    ),
    (
        Family::Launcher,
        &[
            "com.apple.spotlight",
            "com.raycast.macos",
            "com.runningwithcrayons.alfred",
            "org.gnome.shell.extensions",
            "krunner",
            "powertoys.powerlauncher",
        ],
    ),
];

/// Web hosts per family; a host matches with any subdomain. Paths refine a few hosts first (see
/// [`web_family`]).
const HOSTS: &[(Family, &[&str])] = &[
    (
        Family::Mail,
        &[
            "mail.google.com",
            "inbox.google.com",
            "outlook.live.com",
            "outlook.office.com",
            "outlook.office365.com",
            "outlook.cloud.microsoft",
            "mail.yahoo.com",
            "mail.proton.me",
            "mail.protonmail.com",
            "app.fastmail.com",
            "fastmail.com",
            "app.hey.com",
            "mail.superhuman.com",
            "mail.zoho.com",
            "mail.zoho.eu",
            "mail.aol.com",
            "mail.yandex.com",
            "webmail.gandi.net",
            "app.tuta.com",
        ],
    ),
    (
        Family::Chat,
        &[
            "slack.com",
            "teams.microsoft.com",
            "teams.live.com",
            "teams.cloud.microsoft",
            "discord.com",
            "discordapp.com",
            "web.whatsapp.com",
            "web.telegram.org",
            "messenger.com",
            "chat.google.com",
            "messages.google.com",
            "app.element.io",
            "zulipchat.com",
            "app.beeper.com",
            "web.skype.com",
        ],
    ),
    (
        Family::Comments,
        &[
            "github.com",
            "gitlab.com",
            "bitbucket.org",
            "linear.app",
            "atlassian.net",
            "app.asana.com",
            "trello.com",
            "reddit.com",
            "news.ycombinator.com",
            "figma.com",
            "stackoverflow.com",
            "stackexchange.com",
            "superuser.com",
            "youtube.com",
            "app.clickup.com",
            "app.shortcut.com",
            "height.app",
            "producthunt.com",
            "x.com",
            "twitter.com",
            "linkedin.com",
            "facebook.com",
            "lobste.rs",
        ],
    ),
    (
        Family::Docs,
        &[
            "docs.google.com",
            "notion.so",
            "notion.site",
            "coda.io",
            "quip.com",
            "paper.dropbox.com",
            "officeapps.live.com",
            "word.cloud.microsoft",
            "hackmd.io",
            "craft.do",
            "dropbox.com",
            "medium.com",
            "substack.com",
            "overleaf.com",
            "confluence.com",
        ],
    ),
];

fn app_family(s: &Signals) -> Option<Family> {
    if s.app_id.is_empty() {
        return None;
    }
    APP_IDS
        .iter()
        .find(|(_, ids)| ids.contains(&s.app_id.as_str()) || ids.contains(&s.app_id_raw.as_str()))
        .map(|(f, _)| *f)
}

fn web_family(s: &Signals) -> Option<Family> {
    if s.host.is_empty() {
        return None;
    }
    let path = s.path.as_str();
    // Path refinements before plain host matching.
    if host_is(&s.host, "mail.google.com") && path.starts_with("/chat") {
        return Some(Family::Chat);
    }
    if host_is(&s.host, "icloud.com") {
        return path.starts_with("/mail").then_some(Family::Mail);
    }
    if host_is(&s.host, "atlassian.net") && path.starts_with("/wiki") {
        return Some(Family::Docs);
    }
    let messaging = (host_is(&s.host, "linkedin.com") && path.starts_with("/messaging"))
        || ((host_is(&s.host, "x.com") || host_is(&s.host, "twitter.com"))
            && path.starts_with("/messages"))
        || (host_is(&s.host, "facebook.com") && path.starts_with("/messages"))
        || (host_is(&s.host, "instagram.com") && path.starts_with("/direct"));
    if messaging {
        return Some(Family::Chat);
    }
    HOSTS
        .iter()
        .find(|(_, hosts)| hosts.iter().any(|h| host_is(&s.host, h)))
        .map(|(f, _)| *f)
}

/// App name fallbacks for when the platform gives no usable id.
fn name_family(s: &Signals) -> Option<Family> {
    const NAMES: &[(Family, &[&str])] = &[
        (
            Family::Mail,
            &[
                "mail",
                "outlook",
                "thunderbird",
                "spark",
                "mimestream",
                "airmail",
            ],
        ),
        (
            Family::Chat,
            &[
                "slack",
                "microsoft teams",
                "teams",
                "discord",
                "telegram",
                "whatsapp",
                "signal",
                "messages",
                "messenger",
                "element",
                "mattermost",
            ],
        ),
        (Family::Comments, &["linear", "jira"]),
        (
            Family::Docs,
            &[
                "microsoft word",
                "word",
                "pages",
                "notion",
                "obsidian",
                "notes",
                "bear",
            ],
        ),
        (Family::Launcher, &["spotlight", "raycast", "alfred"]),
    ];
    if s.app_name.is_empty() {
        return None;
    }
    NAMES
        .iter()
        .find(|(_, names)| names.contains(&s.app_name.as_str()))
        .map(|(f, _)| *f)
}

fn is_search(s: &Signals) -> bool {
    if s.role.contains("search") {
        return true;
    }
    let t = s.text.as_str();
    const STARTS: &[&str] = &[
        "search", "find ", "find…", "find...", "filter", "jump to", "go to ", "look up",
    ];
    const CONTAINS: &[&str] = &[
        "search or type",
        "address and search",
        "search mail",
        "search messages",
        "type to search",
        "search for",
        "quick find",
    ];
    STARTS.iter().any(|p| t.starts_with(p)) || CONTAINS.iter().any(|p| t.contains(p))
}

/// Strong label/placeholder signals, independent of the app.
fn label_kind(s: &Signals) -> Option<FieldKind> {
    let t = s.text.as_str();
    if t.is_empty() {
        return None;
    }
    const EMAIL: &[&str] = &[
        "message body",
        "email body",
        "mail body",
        "body of the email",
        "compose email",
    ];
    if EMAIL.iter().any(|p| t.contains(p)) {
        return Some(FieldKind::EmailBody);
    }
    const COMMENT: &[&str] = &[
        "comment",
        "leave a reply",
        "write a reply…",
        "add a reply",
        "write a review",
        "review comment",
    ];
    if COMMENT.iter().any(|p| t.contains(p)) {
        return Some(FieldKind::Comment);
    }
    const CHAT: &[&str] = &[
        "type a message",
        "send a message",
        "write a message",
        "type your message",
        "imessage",
        "text message",
        "reply in thread",
        "reply to thread",
        "message in ",
        "start a new conversation",
        "aa message",
    ];
    if CHAT.iter().any(|p| t.contains(p)) {
        return Some(FieldKind::ChatMessage);
    }
    // "Message #general", "Message @sam", "Message Sam Lee" (Slack, Discord, Teams).
    if let Some(rest) = t.strip_prefix("message ")
        && let Some(c) = rest.trim_start().chars().next()
        && (c == '#' || c == '@' || c.is_alphanumeric())
        && !rest.starts_with("body")
        && !rest.starts_with("subject")
    {
        return Some(FieldKind::ChatMessage);
    }
    const DOCUMENT: &[&str] = &[
        "add a description",
        "add description",
        "write a description",
        "type your description",
        "start writing",
        "type '/' for commands",
        "press '/' for commands",
        "write something",
        "tell your story",
    ];
    if DOCUMENT.iter().any(|p| t.contains(p)) {
        return Some(FieldKind::Document);
    }
    None
}

/// Header fields that sit next to a body/composer: subject, recipients, titles.
fn is_header_field(s: &Signals) -> bool {
    if !s.single_line() {
        return false;
    }
    let t = s.text.trim_end_matches([':', '…', '.', ' ']);
    const HEADERS: &[&str] = &[
        "subject",
        "to",
        "cc",
        "bcc",
        "from",
        "title",
        "name",
        "add a title",
        "issue title",
        "summary",
    ];
    HEADERS.contains(&t) || t.starts_with("subject") || t.starts_with("title")
}

/// Labels that mark a plain form input (name, email, phone, address...).
fn is_form_label(s: &Signals) -> bool {
    const FORM: &[&str] = &[
        "first name",
        "last name",
        "full name",
        "your name",
        "email address",
        "e-mail",
        "phone",
        "company",
        "address",
        "city",
        "zip",
        "postal code",
        "country",
        "job title",
        "website",
        "date of birth",
    ];
    FORM.iter().any(|p| s.text.contains(p))
}

/// Decide what kind of field this is from app, role, labels and thread. Pure and fast: the rules
/// are described in the [module docs](self). Secure fields are refused by [`gate`] before this
/// runs.
pub fn classify(ctx: &DraftContext) -> FieldKind {
    let s = Signals::of(ctx);
    let family = app_family(&s)
        .or_else(|| web_family(&s))
        .or_else(|| name_family(&s));

    // 1. Search boxes, including launchers.
    if s.role == "search_field" || is_search(&s) || family == Some(Family::Launcher) {
        return FieldKind::Search;
    }
    // 2. Explicit labels.
    if let Some(kind) = label_kind(&s) {
        return kind;
    }
    // 3. App family.
    if let Some(f) = family {
        if is_header_field(&s) {
            return FieldKind::FormField;
        }
        return match f {
            Family::Mail => FieldKind::EmailBody,
            Family::Chat => FieldKind::ChatMessage,
            Family::Comments => FieldKind::Comment,
            Family::Docs => FieldKind::Document,
            Family::Launcher => FieldKind::Search,
        };
    }
    if is_form_label(&s) {
        return FieldKind::FormField;
    }
    // 4. The conversation around the field.
    if ctx.subject.is_some() || !ctx.recipients.is_empty() {
        return FieldKind::EmailBody;
    }
    if !ctx.thread.is_empty() {
        return FieldKind::ChatMessage;
    }
    // 5. The role alone.
    match s.role.as_str() {
        "text_field" => FieldKind::FormField,
        "document" => FieldKind::Document,
        _ => FieldKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ipc::{AppInfo, FieldInfo, ThreadMessage};

    #[derive(Default)]
    struct Case {
        app_id: Option<&'static str>,
        app_name: &'static str,
        url: Option<&'static str>,
        role: &'static str,
        label: Option<&'static str>,
        placeholder: Option<&'static str>,
        subject: Option<&'static str>,
        recipients: &'static [&'static str],
        thread: bool,
    }

    impl Case {
        fn ctx(&self) -> DraftContext {
            DraftContext {
                app: AppInfo {
                    id: self.app_id.map(str::to_owned),
                    name: self.app_name.to_owned(),
                    window_title: None,
                    url: self.url.map(str::to_owned),
                },
                field: FieldInfo {
                    role: if self.role.is_empty() {
                        "text_area"
                    } else {
                        self.role
                    }
                    .to_owned(),
                    label: self.label.map(str::to_owned),
                    placeholder: self.placeholder.map(str::to_owned),
                    ..Default::default()
                },
                thread: if self.thread {
                    vec![ThreadMessage {
                        author: Some("Sam".into()),
                        text: "Are we still on for Thursday?".into(),
                        ..Default::default()
                    }]
                } else {
                    Vec::new()
                },
                recipients: self.recipients.iter().map(|r| (*r).to_owned()).collect(),
                subject: self.subject.map(str::to_owned),
                screenshot_png_base64: None,
            }
        }
    }

    fn table() -> Vec<(&'static str, Case, FieldKind)> {
        use FieldKind::*;
        vec![
            // --- mail clients ---
            (
                "apple mail body",
                Case {
                    app_id: Some("com.apple.mail"),
                    app_name: "Mail",
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "apple mail subject",
                Case {
                    app_id: Some("com.apple.mail"),
                    role: "text_field",
                    label: Some("Subject:"),
                    ..Default::default()
                },
                FormField,
            ),
            (
                "apple mail to",
                Case {
                    app_id: Some("com.apple.mail"),
                    role: "text_field",
                    label: Some("To:"),
                    ..Default::default()
                },
                FormField,
            ),
            (
                "outlook mac",
                Case {
                    app_id: Some("com.microsoft.Outlook"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "outlook windows exe",
                Case {
                    app_id: Some("OUTLOOK.EXE"),
                    role: "document",
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "new outlook windows",
                Case {
                    app_id: Some("olk.exe"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "thunderbird linux",
                Case {
                    app_id: Some("org.mozilla.Thunderbird.desktop"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "evolution",
                Case {
                    app_id: Some("org.gnome.Evolution"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "gmail web",
                Case {
                    app_id: Some("com.google.Chrome"),
                    url: Some("https://mail.google.com/mail/u/0/#inbox/abc"),
                    role: "web_area",
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "gmail message body label",
                Case {
                    url: Some("https://mail.google.com/mail/u/0/"),
                    label: Some("Message Body"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "gmail subject",
                Case {
                    url: Some("https://mail.google.com/mail/u/0/"),
                    role: "text_field",
                    label: Some("Subject"),
                    ..Default::default()
                },
                FormField,
            ),
            (
                "gmail chat path",
                Case {
                    url: Some("https://mail.google.com/chat/u/0/#chat/space/x"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "outlook web",
                Case {
                    url: Some("https://outlook.office.com/mail/inbox"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "outlook live",
                Case {
                    url: Some("https://outlook.live.com/mail/0/"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "icloud mail path",
                Case {
                    url: Some("https://www.icloud.com/mail/"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "icloud notes is not mail",
                Case {
                    url: Some("https://www.icloud.com/notes/"),
                    ..Default::default()
                },
                Unknown,
            ),
            (
                "fastmail",
                Case {
                    url: Some("https://app.fastmail.com/mail/Inbox"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "proton mail",
                Case {
                    url: Some("https://mail.proton.me/u/0/inbox"),
                    ..Default::default()
                },
                EmailBody,
            ),
            // --- chat apps ---
            (
                "slack mac",
                Case {
                    app_id: Some("com.tinyspeck.slackmacgap"),
                    label: Some("Message #general"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "slack no label",
                Case {
                    app_id: Some("com.tinyspeck.slackmacgap"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "slack windows",
                Case {
                    app_id: Some("slack.exe"),
                    role: "text_field",
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "slack web",
                Case {
                    url: Some("https://app.slack.com/client/T1/C1"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "slack workspace subdomain",
                Case {
                    url: Some("https://acme.slack.com/archives/C1"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "slack thread reply label",
                Case {
                    app_id: Some("com.tinyspeck.slackmacgap"),
                    placeholder: Some("Reply…"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "slack search",
                Case {
                    app_id: Some("com.tinyspeck.slackmacgap"),
                    role: "text_field",
                    placeholder: Some("Search Acme"),
                    ..Default::default()
                },
                Search,
            ),
            (
                "teams new",
                Case {
                    app_id: Some("com.microsoft.teams2"),
                    placeholder: Some("Type a message"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "teams web with subject ignored",
                Case {
                    url: Some("https://teams.microsoft.com/v2/"),
                    subject: Some("Standup"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "teams exe",
                Case {
                    app_id: Some("ms-teams.exe"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "discord",
                Case {
                    app_id: Some("com.hnc.Discord"),
                    placeholder: Some("Message @sam"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "whatsapp web",
                Case {
                    url: Some("https://web.whatsapp.com/"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "telegram desktop linux",
                Case {
                    app_id: Some("org.telegram.desktop"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "signal",
                Case {
                    app_id: Some("org.whispersystems.signal-desktop"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "imessage",
                Case {
                    app_id: Some("com.apple.MobileSMS"),
                    placeholder: Some("iMessage"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "linkedin messaging",
                Case {
                    url: Some("https://www.linkedin.com/messaging/thread/1/"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "x dms",
                Case {
                    url: Some("https://x.com/messages/123"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "chat app by name only",
                Case {
                    app_name: "Slack",
                    ..Default::default()
                },
                ChatMessage,
            ),
            // --- comments ---
            (
                "github pr comment",
                Case {
                    url: Some("https://github.com/o/r/pull/12"),
                    placeholder: Some("Leave a comment"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "github issue body no label",
                Case {
                    url: Some("https://github.com/o/r/issues/3"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "github issue title",
                Case {
                    url: Some("https://github.com/o/r/issues/new"),
                    role: "text_field",
                    label: Some("Title"),
                    ..Default::default()
                },
                FormField,
            ),
            (
                "github search",
                Case {
                    url: Some("https://github.com/o/r"),
                    role: "text_field",
                    placeholder: Some("Search or jump to..."),
                    ..Default::default()
                },
                Search,
            ),
            (
                "gitlab",
                Case {
                    url: Some("https://gitlab.com/g/p/-/merge_requests/9"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "linear web reply",
                Case {
                    url: Some("https://linear.app/acme/issue/ENG-1"),
                    placeholder: Some("Leave a reply..."),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "linear description",
                Case {
                    url: Some("https://linear.app/acme/issue/ENG-1"),
                    placeholder: Some("Add description…"),
                    ..Default::default()
                },
                Document,
            ),
            (
                "linear desktop",
                Case {
                    app_id: Some("com.linear"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "jira cloud",
                Case {
                    url: Some("https://acme.atlassian.net/browse/ENG-1"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "confluence is docs",
                Case {
                    url: Some("https://acme.atlassian.net/wiki/spaces/ENG/pages/1"),
                    ..Default::default()
                },
                Document,
            ),
            (
                "reddit",
                Case {
                    url: Some("https://www.reddit.com/r/rust/comments/x/"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "hacker news",
                Case {
                    url: Some("https://news.ycombinator.com/item?id=1"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "google docs comment box",
                Case {
                    url: Some("https://docs.google.com/document/d/1/edit"),
                    placeholder: Some("Comment or add others with @"),
                    ..Default::default()
                },
                Comment,
            ),
            // --- documents ---
            (
                "google docs canvas",
                Case {
                    url: Some("https://docs.google.com/document/d/1/edit"),
                    role: "document",
                    ..Default::default()
                },
                Document,
            ),
            (
                "notion web",
                Case {
                    url: Some("https://www.notion.so/acme/Page-1"),
                    ..Default::default()
                },
                Document,
            ),
            (
                "notion comment",
                Case {
                    app_id: Some("notion.id"),
                    placeholder: Some("Add a comment…"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "word windows",
                Case {
                    app_id: Some("WINWORD.EXE"),
                    role: "document",
                    ..Default::default()
                },
                Document,
            ),
            (
                "pages",
                Case {
                    app_id: Some("com.apple.iWork.Pages"),
                    ..Default::default()
                },
                Document,
            ),
            (
                "obsidian",
                Case {
                    app_id: Some("md.obsidian"),
                    ..Default::default()
                },
                Document,
            ),
            (
                "apple notes",
                Case {
                    app_id: Some("com.apple.Notes"),
                    ..Default::default()
                },
                Document,
            ),
            (
                "notion type slash",
                Case {
                    placeholder: Some("Press '/' for commands"),
                    ..Default::default()
                },
                Document,
            ),
            // --- search ---
            (
                "search role",
                Case {
                    role: "search_field",
                    ..Default::default()
                },
                Search,
            ),
            (
                "search placeholder",
                Case {
                    role: "text_field",
                    placeholder: Some("Search mail"),
                    ..Default::default()
                },
                Search,
            ),
            (
                "browser omnibox",
                Case {
                    app_id: Some("com.google.Chrome"),
                    role: "text_field",
                    label: Some("Address and search bar"),
                    ..Default::default()
                },
                Search,
            ),
            (
                "spotlight",
                Case {
                    app_id: Some("com.apple.Spotlight"),
                    role: "text_field",
                    ..Default::default()
                },
                Search,
            ),
            (
                "raycast",
                Case {
                    app_id: Some("com.raycast.macos"),
                    ..Default::default()
                },
                Search,
            ),
            // --- forms and context fallbacks ---
            (
                "generic single line",
                Case {
                    role: "text_field",
                    ..Default::default()
                },
                FormField,
            ),
            (
                "form email address",
                Case {
                    label: Some("Email address"),
                    ..Default::default()
                },
                FormField,
            ),
            (
                "form first name",
                Case {
                    role: "text_field",
                    label: Some("First name"),
                    ..Default::default()
                },
                FormField,
            ),
            (
                "unknown app with subject",
                Case {
                    app_id: Some("com.example.crm"),
                    subject: Some("Re: Invoice"),
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "unknown app with recipients",
                Case {
                    app_id: Some("com.example.crm"),
                    recipients: &["sam@example.com"],
                    ..Default::default()
                },
                EmailBody,
            ),
            (
                "unknown app with thread",
                Case {
                    app_id: Some("com.example.support"),
                    thread: true,
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "unknown web app type a message",
                Case {
                    url: Some("https://support.example.com/"),
                    placeholder: Some("Type your message here"),
                    ..Default::default()
                },
                ChatMessage,
            ),
            (
                "unknown web app comment label",
                Case {
                    url: Some("https://forum.example.com/t/1"),
                    label: Some("Write a comment"),
                    ..Default::default()
                },
                Comment,
            ),
            (
                "unknown text area",
                Case {
                    ..Default::default()
                },
                Unknown,
            ),
            (
                "unknown document role",
                Case {
                    role: "document",
                    ..Default::default()
                },
                Document,
            ),
            (
                "message body label wins over chat family",
                Case {
                    app_id: Some("com.tinyspeck.slackmacgap"),
                    label: Some("Message body"),
                    ..Default::default()
                },
                EmailBody,
            ),
        ]
    }

    #[test]
    fn classify_table() {
        let cases = table();
        assert!(cases.len() >= 40, "only {} cases", cases.len());
        let mut failures = Vec::new();
        for (name, case, want) in &cases {
            let got = classify(&case.ctx());
            if got != *want {
                failures.push(format!("{name}: got {got:?}, want {want:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn stub_semantics_are_kept() {
        // The M3 contract stub's decisions still hold for the inputs it understood.
        let mut ctx = DraftContext::default();
        ctx.field.role = "search_field".into();
        assert_eq!(classify(&ctx), FieldKind::Search);
        ctx.field.role = "text_area".into();
        assert_eq!(classify(&ctx), FieldKind::Unknown);
        ctx.thread.push(ThreadMessage::default());
        assert_eq!(classify(&ctx), FieldKind::ChatMessage);
        ctx.subject = Some("Hi".into());
        assert_eq!(classify(&ctx), FieldKind::EmailBody);
    }

    #[test]
    fn secure_fields_are_refused_before_classification() {
        let excluded: Vec<String> = Vec::new();
        let mut ctx = Case {
            app_id: Some("com.tinyspeck.slackmacgap"),
            ..Default::default()
        }
        .ctx();
        ctx.field.secure = true;
        assert_eq!(gate(&ctx, &excluded), Err(Refusal::SecureField));
        for (role, label) in [
            ("secure_text_field", None),
            ("text_field", Some("Password")),
            ("text_field", Some("Enter the 6-digit verification code")),
            ("text_field", Some("PIN")),
            ("text_field", Some("Card number")),
            ("text_field", Some("CVV")),
            ("text_field", Some("One-time code")),
        ] {
            let mut ctx = Case::default().ctx();
            ctx.field.role = role.into();
            ctx.field.label = label.map(str::to_owned);
            assert_eq!(
                gate(&ctx, &excluded),
                Err(Refusal::SecureField),
                "{role} {label:?}"
            );
        }
        // "Pinned messages" is not a PIN.
        let mut ctx = Case {
            app_id: Some("slack"),
            ..Default::default()
        }
        .ctx();
        ctx.field.label = Some("Pinned messages note".into());
        assert_eq!(gate(&ctx, &excluded), Ok(FieldKind::ChatMessage));
    }

    #[test]
    fn excluded_apps_and_hosts_are_refused() {
        let excluded: Vec<String> = DEFAULT_EXCLUDED_APPS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let ctx = Case {
            app_id: Some("com.1password.1password"),
            app_name: "1Password",
            ..Default::default()
        }
        .ctx();
        assert_eq!(
            gate(&ctx, &excluded),
            Err(Refusal::ExcludedApp("1Password".into()))
        );
        let ctx = Case {
            app_id: Some("Bitwarden.exe"),
            ..Default::default()
        }
        .ctx();
        assert!(matches!(
            gate(&ctx, &excluded),
            Err(Refusal::ExcludedApp(_))
        ));
        let ctx = Case {
            url: Some("https://vault.bitwarden.com/#/vault"),
            ..Default::default()
        }
        .ctx();
        assert!(matches!(
            gate(&ctx, &excluded),
            Err(Refusal::ExcludedApp(_))
        ));
        // A user-added bank host matches subdomains.
        let ctx = Case {
            url: Some("https://online.mybank.example/"),
            ..Default::default()
        }
        .ctx();
        assert!(matches!(
            gate(&ctx, &["mybank.example".into()]),
            Err(Refusal::ExcludedApp(_))
        ));
        // ...but not look-alike hosts.
        let ctx = Case {
            url: Some("https://notmybank.example/"),
            thread: true,
            ..Default::default()
        }
        .ctx();
        assert_eq!(
            gate(&ctx, &["mybank.example".into()]),
            Ok(FieldKind::ChatMessage)
        );
    }

    #[test]
    fn more_password_managers_are_excluded() {
        let excluded: Vec<String> = DEFAULT_EXCLUDED_APPS
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        for id in [
            "me.proton.pass.electron",
            "ProtonPass.exe",
            "proton-pass",
            "in.sinew.Enpass-Desktop",
            "Enpass.exe",
            "enpass.desktop",
            "com.nordsec.nordpass",
            "NordPass.exe",
            "nordpass",
        ] {
            let ctx = Case {
                app_id: Some(id),
                thread: true,
                ..Default::default()
            }
            .ctx();
            assert!(
                matches!(gate(&ctx, &excluded), Err(Refusal::ExcludedApp(_))),
                "{id}"
            );
        }
        for url in [
            "https://pass.proton.me/u/0",
            "https://app.nordpass.com/vault",
        ] {
            let ctx = Case {
                url: Some(url),
                thread: true,
                ..Default::default()
            }
            .ctx();
            assert!(
                matches!(gate(&ctx, &excluded), Err(Refusal::ExcludedApp(_))),
                "{url}"
            );
        }
        // Other Proton apps are not password managers.
        let ctx = Case {
            url: Some("https://mail.proton.me/u/0/inbox"),
            thread: true,
            ..Default::default()
        }
        .ctx();
        assert!(gate(&ctx, &excluded).is_ok());
    }

    #[test]
    fn terminals_are_refused_unless_allowed() {
        for id in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "WindowsTerminal.exe",
            r"C:\Windows\System32\conhost.exe",
            "cmd.exe",
            "powershell.exe",
            "pwsh.exe",
            "org.gnome.Terminal",
            "org.gnome.Terminal.desktop",
            "org.kde.konsole",
            "Alacritty",
            "kitty",
            "wezterm-gui.exe",
            "com.mitchellh.ghostty",
            // Security review PA-5: the rest of the review's list.
            "dev.warp.Warp-Stable",
            "com.gexperts.Tilix",
            "org.gnome.Ptyxis.desktop",
            "xfce4-terminal",
            "mate-terminal",
            "xterm",
        ] {
            let ctx = Case {
                app_id: Some(id),
                app_name: "Term",
                thread: true,
                ..Default::default()
            }
            .ctx();
            assert!(is_terminal(&ctx), "{id}");
            assert_eq!(
                gate(&ctx, &[]),
                Err(Refusal::Terminal("Term".into())),
                "{id}"
            );
            assert!(gate_with(&ctx, &[], true).is_ok(), "{id}");
        }
        // Secure fields stay refused even where terminals are allowed.
        let mut ctx = Case {
            app_id: Some("com.apple.Terminal"),
            ..Default::default()
        }
        .ctx();
        ctx.field.secure = true;
        assert_eq!(gate_with(&ctx, &[], true), Err(Refusal::SecureField));
        // Not a terminal.
        let ctx = Case {
            app_id: Some("com.tinyspeck.slackmacgap"),
            thread: true,
            ..Default::default()
        }
        .ctx();
        assert!(!is_terminal(&ctx));
        let err: nugget_core::Error = Refusal::Terminal("Terminal".into()).into();
        assert!(matches!(err, nugget_core::Error::Invalid(_)));
    }

    /// Security review PA-5 (`security-m3-m6-poc/draft_poc.rs`, inverted): terminals and the
    /// password managers a11y names are refused by the default lists.
    #[test]
    fn pa5_terminals_and_other_password_managers_are_refused() {
        let excluded: Vec<String> = DEFAULT_EXCLUDED_APPS
            .iter()
            .map(|s| s.to_string())
            .collect();
        for (id, name) in [
            ("com.apple.Terminal", "Terminal"),
            ("com.googlecode.iterm2", "iTerm2"),
            ("org.gnome.Terminal.desktop", "Terminal"),
            ("WindowsTerminal.exe", "Windows Terminal"),
            ("me.proton.pass.electron", "Proton Pass"),
            ("in.sinew.Enpass-Desktop", "Enpass"),
            ("com.nordsec.nordpass", "NordPass"),
            (
                "com.keepersecurity.passwordmanager",
                "Keeper Password Manager",
            ),
            ("KeeperPasswordManager.exe", "Keeper"),
            ("com.siber.RoboForm", "RoboForm"),
            ("RoboForm.exe", "RoboForm"),
            // Unknown id, but named a terminal.
            ("com.example.shell", "GNOME Terminal"),
        ] {
            let ctx = Case {
                app_id: Some(id),
                app_name: name,
                thread: true,
                ..Default::default()
            }
            .ctx();
            assert!(gate(&ctx, &excluded).is_err(), "{id}");
        }
        // The word, not a substring of another one.
        let ctx = Case {
            app_id: Some("com.example.notes"),
            app_name: "Terminally Online Notes",
            ..Default::default()
        }
        .ctx();
        assert!(!is_terminal(&ctx));
    }

    #[test]
    fn search_is_not_draftable() {
        let ctx = Case {
            role: "search_field",
            ..Default::default()
        }
        .ctx();
        assert_eq!(
            gate(&ctx, &[]),
            Err(Refusal::NotDraftable(FieldKind::Search))
        );
        let err: nugget_core::Error = Refusal::NotDraftable(FieldKind::Search).into();
        assert!(matches!(err, nugget_core::Error::Unsupported(_)));
        let err: nugget_core::Error = Refusal::SecureField.into();
        assert!(matches!(err, nugget_core::Error::Invalid(_)));
    }

    #[test]
    fn url_and_app_id_normalisation() {
        assert_eq!(
            url_host_path("https://user@Mail.Google.com:443/mail/u/0?x=1#inbox"),
            ("mail.google.com".into(), "/mail/u/0".into())
        );
        assert_eq!(
            url_host_path("app.slack.com/client"),
            ("app.slack.com".into(), "/client".into())
        );
        assert_eq!(url_host_path(""), (String::new(), String::new()));
        assert_eq!(
            normalize_app_id("C:\\Program Files\\Slack\\Slack.exe"),
            "slack"
        );
        assert_eq!(
            normalize_app_id("org.gnome.Evolution.desktop"),
            "org.gnome.evolution"
        );
        assert!(host_is("acme.slack.com", "slack.com"));
        assert!(!host_is("evilslack.com", "slack.com"));
        assert!(!host_is("", "slack.com"));
    }
}
