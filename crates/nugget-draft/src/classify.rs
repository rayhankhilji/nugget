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

/// Terminal emulators (comma-separated app ids): refused by default, since text typed into a
/// shell can run as a command, unless `DraftConfig::allow_terminals`.
pub const DEFAULT_TERMINAL_APPS: &str = "\
com.apple.terminal, com.googlecode.iterm2, com.mitchellh.ghostty, com.github.wez.wezterm, \
org.alacritty, io.alacritty, net.kovidgoyal.kitty, windowsterminal, openconsole, conhost, \
cmd, powershell, powershell_ise, pwsh, org.gnome.terminal, gnome-terminal, \
gnome-terminal-server, org.gnome.console, kgx, org.kde.konsole, konsole, alacritty, kitty, \
wezterm, wezterm-gui, org.wezfurlong.wezterm, ghostty, xterm, uxterm, dev.warp.warp-stable, \
dev.warp.warp, warp, com.gexperts.tilix, tilix, org.gnome.ptyxis, ptyxis, terminator, foot, \
footclient, rxvt, urxvt, co.zeit.hyper, hyper, org.tabby, tabby, putty, kitty.exe, mintty, \
mobaxterm, termius, com.termius-dmg.mac, securecrt, com.vandyke.securecrt, guake, tilda, \
yakuake, cool-retro-term, terminus, contour, rio, com.raphaelamorim.rio";

/// Whether the focused app is a terminal emulator ([`DEFAULT_TERMINAL_APPS`]).
/// Also any app whose id contains `terminal` (`xfce4-terminal`, `mate-terminal`), or whose name
/// has the word "Terminal". Also a terminal inside another app (QA GG1-3): an accessibility role
/// `terminal`, or a field whose label starts with "Terminal" (VS Code's integrated terminal is
/// an xterm.js textarea labelled "Terminal 1, bash") or names xterm.
pub fn is_terminal(ctx: &DraftContext) -> bool {
    let s = Signals::of(ctx);
    (!s.app_id.is_empty()
        && (listed(DEFAULT_TERMINAL_APPS).any(|t| s.app_id == t || s.app_id_raw == t)
            || s.app_id.contains("terminal")))
        || words(&s.app_name).any(|w| w == "terminal")
        || s.role == "terminal"
        || words(&s.text).next() == Some("terminal")
        || s.text.contains("xterm")
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
    // A multi-line field labelled "Message #it-password-help" is a chat, not a credential field
    // (QA GG1-26): the label rule applies to single-line and unknown fields only.
    if matches!(role.as_str(), "text_area" | "document" | "web_area") {
        return false;
    }
    let text = field_text(ctx);
    SECURE_PHRASES.iter().any(|p| text.contains(p))
        || words(&text).any(|w| {
            SECURE_WORDS.contains(&w)
                // "cvv2", "cvc2", "cvn".
                || ["cvv", "cvc", "cvn"].iter().any(|p| w.starts_with(p))
        })
}

/// Label and placeholder phrases (lower case) that name a credential, in the UI languages
/// (English, Arabic, German, Spanish, French, Hindi, Japanese, Portuguese, Chinese).
const SECURE_PHRASES: &[&str] = &[
    // Passwords.
    "password",
    "passphrase",
    "passcode",
    "passwort",
    "kennwort",
    "contraseña",
    "contrasena",
    "mot de passe",
    "senha",
    "palavra-passe",
    "كلمة المرور",
    "كلمة السر",
    "पासवर्ड",
    "パスワード",
    "暗証番号",
    "密码",
    "口令",
    // One-time and verification codes.
    "one-time code",
    "one time code",
    "verification code",
    "security code",
    "authentication code",
    "2fa code",
    "digit code",
    "code we sent",
    "code we texted",
    "code we emailed",
    "code that we sent",
    "enter the code",
    "enter code",
    "bestätigungscode",
    "verifizierungscode",
    "sicherheitscode",
    "einmalcode",
    "código de verificación",
    "código de seguridad",
    "código de verificação",
    "código de segurança",
    "code de vérification",
    "code de sécurité",
    "code à usage unique",
    "رمز التحقق",
    "رمز الأمان",
    "كود التحقق",
    "सत्यापन कोड",
    "ओटीपी",
    "認証コード",
    "確認コード",
    "ワンタイム",
    "セキュリティコード",
    "验证码",
    "校验码",
    "安全码",
    // Cards, accounts and identity numbers.
    "card number",
    "credit card",
    "kartennummer",
    "kreditkarte",
    "número de tarjeta",
    "numero de tarjeta",
    "numéro de carte",
    "número do cartão",
    "رقم البطاقة",
    "कार्ड नंबर",
    "カード番号",
    "卡号",
    "信用卡",
    "social security",
    // Keys.
    "recovery key",
    "secret key",
    "private key",
    "api key",
];

/// Whole label words (lower case) that name a credential.
const SECURE_WORDS: &[&str] = &[
    "pin",
    "otp",
    "ssn",
    "mfa",
    "totp",
    "iban",
    "geheimzahl",
    "nip",
    "पिन",
];

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

/// The entries of a comma-separated list.
fn listed(list: &str) -> impl Iterator<Item = &str> {
    list.split(',').map(str::trim)
}

/// App ids (normalised) per family.
const APP_IDS: &[(Family, &str)] = &[
    (
        Family::Mail,
        "\
com.apple.mail, com.microsoft.outlook, outlook, olk, org.mozilla.thunderbird, \
thunderbird, net.thunderbird.thunderbird, org.gnome.evolution, evolution, \
org.gnome.geary, geary, com.readdle.smartemail-mac, com.readdle.spark, \
it.bloop.airmail2, com.mimestream.mimestream, com.superhuman.electron, superhuman, \
com.hey.app.desktop, com.getmailspring.mailspring, mailspring, mailclient, \
com.canary.mail, io.canarymail.mac, org.kde.kmail2, kmail, proton mail, \
ch.protonmail.desktop",
    ),
    (
        Family::Chat,
        "\
com.tinyspeck.slackmacgap, com.slack.slack, slack, com.microsoft.teams, \
com.microsoft.teams2, teams, ms-teams, msteams, com.hnc.discord, \
com.discordapp.discord, discord, ru.keepcoder.telegram, org.telegram.desktop, \
telegramdesktop, telegram, net.whatsapp.whatsapp, whatsapp, desktop.whatsapp, \
org.whispersystems.signal-desktop, org.signal.signal, signal, com.apple.mobilesms, \
com.apple.ichat, com.facebook.archon, messenger, im.riot.riot, element, \
com.mattermost.desktop, mattermost, org.zulip.zulip-electron, zulip, \
com.tencent.xinwechat, wechat, jp.naver.line.mac, line, com.skype.skype, skype, \
us.zoom.xos, zoom, com.google.chat, com.rocket.chat, rocket.chat, beeper, \
com.automattic.beeper.desktop",
    ),
    (
        Family::Comments,
        "\
com.linear, linear, com.atlassian.jira, com.asana.app, asana, com.figma.desktop, figma, \
com.clickup.desktop-app",
    ),
    (
        Family::Docs,
        "\
com.microsoft.word, winword, com.apple.iwork.pages, notion.id, notion, md.obsidian, \
obsidian, com.apple.notes, net.shinyfrog.bear, com.apple.textedit, \
org.libreoffice.libreoffice, libreoffice-writer, soffice, swriter, notepad, \
org.gnome.texteditor, org.gnome.gedit, gedit, abnerworks.typora, typora, \
com.lukilabs.lukiapp, com.microsoft.onenote.mac, onenote, com.evernote.evernote, \
evernote, com.ulyssesapp.mac, com.coda.desktop",
    ),
    (
        Family::Launcher,
        "\
com.apple.spotlight, com.raycast.macos, com.runningwithcrayons.alfred, \
org.gnome.shell.extensions, krunner, powertoys.powerlauncher",
    ),
];

/// Web hosts per family; a host matches with any subdomain. Paths refine a few hosts first (see
/// [`web_family`]).
const HOSTS: &[(Family, &str)] = &[
    (
        Family::Mail,
        "\
mail.google.com, inbox.google.com, outlook.live.com, outlook.office.com, \
outlook.office365.com, outlook.cloud.microsoft, mail.yahoo.com, mail.proton.me, \
mail.protonmail.com, app.fastmail.com, fastmail.com, app.hey.com, mail.superhuman.com, \
mail.zoho.com, mail.zoho.eu, mail.aol.com, mail.yandex.com, webmail.gandi.net, \
app.tuta.com",
    ),
    (
        Family::Chat,
        "\
slack.com, teams.microsoft.com, teams.live.com, teams.cloud.microsoft, discord.com, \
discordapp.com, web.whatsapp.com, web.telegram.org, messenger.com, chat.google.com, \
messages.google.com, app.element.io, zulipchat.com, app.beeper.com, web.skype.com",
    ),
    (
        Family::Comments,
        "\
github.com, gitlab.com, bitbucket.org, linear.app, atlassian.net, app.asana.com, \
trello.com, reddit.com, news.ycombinator.com, figma.com, stackoverflow.com, \
stackexchange.com, superuser.com, youtube.com, app.clickup.com, app.shortcut.com, \
height.app, producthunt.com, x.com, twitter.com, linkedin.com, facebook.com, lobste.rs",
    ),
    (
        Family::Docs,
        "\
docs.google.com, notion.so, notion.site, coda.io, quip.com, paper.dropbox.com, \
officeapps.live.com, word.cloud.microsoft, hackmd.io, craft.do, dropbox.com, \
medium.com, substack.com, overleaf.com, confluence.com",
    ),
];

fn app_family(s: &Signals) -> Option<Family> {
    if s.app_id.is_empty() {
        return None;
    }
    APP_IDS
        .iter()
        .find(|(_, ids)| listed(ids).any(|id| id == s.app_id || id == s.app_id_raw))
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
        .find(|(_, hosts)| listed(hosts).any(|h| host_is(&s.host, h)))
        .map(|(f, _)| *f)
}

/// App name fallbacks for when the platform gives no usable id.
fn name_family(s: &Signals) -> Option<Family> {
    const NAMES: &[(Family, &str)] = &[
        (
            Family::Mail,
            "mail, outlook, thunderbird, spark, mimestream, airmail",
        ),
        (
            Family::Chat,
            "slack, microsoft teams, teams, discord, telegram, whatsapp, signal, messages, messenger, element, mattermost",
        ),
        (Family::Comments, "linear, jira"),
        (
            Family::Docs,
            "microsoft word, word, pages, notion, obsidian, notes, bear",
        ),
        (Family::Launcher, "spotlight, raycast, alfred"),
    ];
    if s.app_name.is_empty() {
        return None;
    }
    NAMES
        .iter()
        .find(|(_, names)| listed(names).any(|n| n == s.app_name))
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
    // The UI languages (QA GG1-14): "In E-Mails suchen", "Buscar", "Rechercher"...
    const LOCAL: &[&str] = &[
        "suchen",
        "suche",
        "buscar",
        "búsqueda",
        "rechercher",
        "recherche",
        "pesquisar",
        "procurar",
        "zoeken",
        "cerca",
        "بحث",
        "ابحث",
        "खोज",
        "検索",
        "搜索",
        "查找",
    ];
    STARTS.iter().any(|p| t.starts_with(p))
        || CONTAINS.iter().any(|p| t.contains(p))
        || ((s.single_line() || s.role.is_empty()) && words(t).any(|w| LOCAL.contains(&w)))
        || LOCAL.iter().any(|p| !p.is_ascii() && t.starts_with(p))
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
    // "To recipients", "Add a subject" (QA GG1-25).
    let first = words(t).next().unwrap_or("");
    HEADERS.contains(&t)
        || t.starts_with("subject")
        || t.starts_with("title")
        || matches!(first, "to" | "cc" | "bcc" | "recipients")
        || t.starts_with("add a subject")
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
    use nugget_core::ipc::ThreadMessage;

    /// A context from app id, page URL, role (default `text_area`) and label; "" means unset.
    fn ctx(app_id: &str, url: &str, role: &str, label: &str) -> DraftContext {
        let some = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let mut c = DraftContext::default();
        c.app.id = some(app_id);
        c.app.url = some(url);
        c.field.role = if role.is_empty() { "text_area" } else { role }.into();
        c.field.label = some(label);
        c
    }

    fn defaults() -> Vec<String> {
        DEFAULT_EXCLUDED_APPS
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    /// `app id | url | role | label | kind` per line; empty means unset.
    const CASES: &str = "\
com.apple.mail||||email_body
com.apple.mail||text_field|Subject:|form_field
com.apple.mail||text_field|To:|form_field
OUTLOOK.EXE||document||email_body
olk.exe||||email_body
org.mozilla.Thunderbird.desktop||||email_body
com.google.Chrome|https://mail.google.com/mail/u/0/#inbox/abc|web_area||email_body
|https://mail.google.com/mail/u/0/||Message Body|email_body
|https://mail.google.com/mail/u/0/|text_field|Subject|form_field
|https://mail.google.com/chat/u/0/#chat/space/x|||chat_message
|https://outlook.office.com/mail/inbox|||email_body
|https://www.icloud.com/mail/|||email_body
|https://www.icloud.com/notes/|||unknown
|https://mail.proton.me/u/0/inbox|||email_body
com.tinyspeck.slackmacgap|||Message #general|chat_message
com.tinyspeck.slackmacgap||||chat_message
slack.exe||text_field||chat_message
|https://acme.slack.com/archives/C1|||chat_message
com.tinyspeck.slackmacgap|||Reply…|chat_message
com.tinyspeck.slackmacgap||text_field|Search Acme|search
com.microsoft.teams2|||Type a message|chat_message
ms-teams.exe||||chat_message
com.hnc.Discord|||Message @sam|chat_message
|https://web.whatsapp.com/|||chat_message
com.apple.MobileSMS|||iMessage|chat_message
|https://www.linkedin.com/messaging/thread/1/|||chat_message
|https://x.com/messages/123|||chat_message
|https://github.com/o/r/pull/12||Leave a comment|comment
|https://github.com/o/r/issues/3|||comment
|https://github.com/o/r/issues/new|text_field|Title|form_field
|https://github.com/o/r|text_field|Search or jump to...|search
|https://linear.app/acme/issue/ENG-1||Leave a reply...|comment
|https://linear.app/acme/issue/ENG-1||Add description…|document
|https://acme.atlassian.net/browse/ENG-1|||comment
|https://acme.atlassian.net/wiki/spaces/ENG/pages/1|||document
|https://docs.google.com/document/d/1/edit||Comment or add others with @|comment
|https://docs.google.com/document/d/1/edit|document||document
notion.id|||Add a comment…|comment
WINWORD.EXE||document||document
com.apple.Notes||||document
|||Press '/' for commands|document
||search_field||search
||text_field|Search mail|search
com.google.Chrome||text_field|Address and search bar|search
com.raycast.macos||||search
||text_field||form_field
|||Email address|form_field
|https://support.example.com/||Type your message here|chat_message
|https://forum.example.com/t/1||Write a comment|comment
||||unknown
||document||document
com.tinyspeck.slackmacgap|||Message body|email_body
|https://mail.google.com/mail/u/0/|text_field|In E-Mails suchen|search
|https://outlook.office.com/mail/inbox|text_field|Buscar|search
|https://mail.google.com/mail/u/0/|combo_box|To recipients|form_field
|https://outlook.office.com/mail/inbox|text_field|Add a subject|form_field";

    #[test]
    fn classify_table() {
        use FieldKind::*;
        let kinds = [
            EmailBody,
            ChatMessage,
            Comment,
            Document,
            FormField,
            Search,
            Unknown,
        ];
        let failures: Vec<String> = CASES
            .lines()
            .filter_map(|line| {
                let f: Vec<&str> = line.split('|').collect();
                let want = kinds.into_iter().find(|k| k.as_str() == f[4]).unwrap();
                let got = classify(&ctx(f[0], f[1], f[2], f[3]));
                (got != want).then(|| format!("{line}: got {got:?}"))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));

        // Name only, conversation fallbacks, and a chat host ignoring a subject.
        let mut c = ctx("", "", "", "");
        c.app.name = "Slack".into();
        assert_eq!(classify(&c), ChatMessage);
        let mut c = ctx("", "https://teams.microsoft.com/v2/", "", "");
        c.subject = Some("Standup".into());
        assert_eq!(classify(&c), ChatMessage);
        let mut c = ctx("com.example.crm", "", "", "");
        c.thread.push(ThreadMessage::default());
        assert_eq!(classify(&c), ChatMessage);
        c.recipients = vec!["sam@example.com".into()];
        assert_eq!(classify(&c), EmailBody);
        c.recipients.clear();
        c.subject = Some("Re: Invoice".into());
        assert_eq!(classify(&c), EmailBody);
    }

    #[test]
    fn secure_fields_are_refused_before_classification() {
        let mut c = ctx("com.tinyspeck.slackmacgap", "", "", "");
        c.field.secure = true;
        assert_eq!(gate(&c, &[]), Err(Refusal::SecureField));
        for (role, label) in [
            ("secure_text_field", ""),
            ("text_field", "Password"),
            ("text_field", "Enter the 6-digit verification code"),
            ("text_field", "PIN"),
            ("text_field", "Card number"),
            ("text_field", "CVV"),
            ("text_field", "One-time code"),
            // QA GG1-2 and GG1-10: the UI languages, and common real labels.
            ("text_field", "Contraseña"),
            ("text_field", "Mot de passe"),
            ("text_field", "Kennwort"),
            ("text_field", "Senha"),
            ("text_field", "كلمة المرور"),
            ("text_field", "पासवर्ड"),
            ("text_field", "パスワード"),
            ("text_field", "密码"),
            ("text_field", "Enter the 6-digit code we texted you"),
            ("text_field", "CVV2"),
            ("text_field", "Kartennummer"),
            ("text_field", "Código de verificación"),
            ("text_field", "رمز التحقق"),
            ("text_field", "認証コード"),
            ("text_field", "验证码"),
            ("combo_box", "Numéro de carte"),
        ] {
            let c = ctx("", "", role, label);
            assert_eq!(gate(&c, &[]), Err(Refusal::SecureField), "{role} {label}");
        }
        // "Pinned messages" is not a PIN.
        let c = ctx("slack", "", "", "Pinned messages note");
        assert_eq!(gate(&c, &[]), Ok(FieldKind::ChatMessage));
        // QA GG1-26: a chat named after a credential word is a chat.
        let c = ctx("slack", "", "text_area", "Message #it-password-help");
        assert_eq!(gate(&c, &[]), Ok(FieldKind::ChatMessage));
    }

    #[test]
    fn excluded_apps_and_hosts_are_refused() {
        let excluded = defaults();
        let mut c = ctx("com.1password.1password", "", "", "");
        c.app.name = "1Password".into();
        assert_eq!(
            gate(&c, &excluded),
            Err(Refusal::ExcludedApp("1Password".into()))
        );
        for (id, url) in [
            ("Bitwarden.exe", ""),
            ("", "https://vault.bitwarden.com/#/vault"),
            ("me.proton.pass.electron", ""),
            ("ProtonPass.exe", ""),
            ("proton-pass", ""),
            ("in.sinew.Enpass-Desktop", ""),
            ("Enpass.exe", ""),
            ("enpass.desktop", ""),
            ("com.nordsec.nordpass", ""),
            ("NordPass.exe", ""),
            ("com.keepersecurity.passwordmanager", ""),
            ("KeeperPasswordManager.exe", ""),
            ("com.siber.RoboForm", ""),
            ("RoboForm.exe", ""),
            ("", "https://pass.proton.me/u/0"),
            ("", "https://app.nordpass.com/vault"),
        ] {
            let mut c = ctx(id, url, "", "");
            c.thread.push(ThreadMessage::default());
            assert!(
                matches!(gate(&c, &excluded), Err(Refusal::ExcludedApp(_))),
                "{id}{url}"
            );
        }
        // Other Proton apps are not password managers.
        let mut c = ctx("", "https://mail.proton.me/u/0/inbox", "", "");
        c.thread.push(ThreadMessage::default());
        assert!(gate(&c, &excluded).is_ok());
        // A user-added host matches subdomains, but not look-alikes.
        let bank = ["mybank.example".to_owned()];
        let c = ctx("", "https://online.mybank.example/", "", "");
        assert!(matches!(gate(&c, &bank), Err(Refusal::ExcludedApp(_))));
        let mut c = ctx("", "https://notmybank.example/", "", "");
        c.thread.push(ThreadMessage::default());
        assert_eq!(gate(&c, &bank), Ok(FieldKind::ChatMessage));
    }

    /// Security review PA-5: terminals are refused unless allowed.
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
            "org.gnome.Terminal.desktop",
            "org.kde.konsole",
            "Alacritty",
            "kitty",
            "wezterm-gui.exe",
            "com.mitchellh.ghostty",
            "dev.warp.Warp-Stable",
            "com.gexperts.Tilix",
            "org.gnome.Ptyxis.desktop",
            "xfce4-terminal",
            "mate-terminal",
            "xterm",
            "putty.exe",
        ] {
            let mut c = ctx(id, "", "", "");
            c.app.name = "Term".into();
            c.thread.push(ThreadMessage::default());
            assert!(is_terminal(&c), "{id}");
            assert_eq!(gate(&c, &[]), Err(Refusal::Terminal("Term".into())), "{id}");
            assert!(gate_with(&c, &[], true).is_ok(), "{id}");
        }
        // An unknown id named a terminal; but not a word that merely starts with it.
        let mut c = ctx("com.example.shell", "", "", "");
        c.app.name = "GNOME Terminal".into();
        assert!(is_terminal(&c));
        c.app.name = "Terminally Online Notes".into();
        assert!(!is_terminal(&c));
        assert!(!is_terminal(&ctx("com.tinyspeck.slackmacgap", "", "", "")));
        // QA GG1-3: VS Code's integrated terminal.
        let c = ctx("com.microsoft.VSCode", "", "text_area", "Terminal 1, bash");
        assert!(is_terminal(&c));
        // Secure fields stay refused even where terminals are allowed.
        let mut c = ctx("com.apple.Terminal", "", "", "");
        c.field.secure = true;
        assert_eq!(gate_with(&c, &[], true), Err(Refusal::SecureField));
    }

    #[test]
    fn search_is_not_draftable_and_refusals_map_to_errors() {
        let c = ctx("", "", "search_field", "");
        assert_eq!(gate(&c, &[]), Err(Refusal::NotDraftable(FieldKind::Search)));
        let err: nugget_core::Error = Refusal::NotDraftable(FieldKind::Search).into();
        assert!(matches!(err, nugget_core::Error::Unsupported(_)));
        for r in [Refusal::SecureField, Refusal::Terminal("T".into())] {
            assert!(matches!(
                nugget_core::Error::from(r),
                nugget_core::Error::Invalid(_)
            ));
        }
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
