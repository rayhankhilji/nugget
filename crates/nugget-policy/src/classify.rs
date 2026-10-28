//! Deterministic risk classifier. It can only raise the declared risk, never lower it.
//!
//! Signals, all table-driven and allocation-light (a few microseconds per action):
//! - the executor verb (`send_email`, `delete_file`, `submit`, ...);
//! - the target label (button text, link text), in English, Spanish, French, German, Italian,
//!   Portuguese, Dutch, Polish, Turkish, Russian, Chinese, Japanese, Korean, Arabic and Hindi,
//!   after Unicode folding (NFKC, invisible characters stripped, Latin look-alikes folded);
//! - prices in a label ("Continue – $49.99");
//! - key presses (Ctrl/Cmd+Enter sends, Shift+Delete destroys, Enter in a terminal runs code);
//! - typed text (secure fields, card numbers, newline-terminated chat messages, shell commands);
//! - URLs and deep links in labels, typed text and the page URL;
//! - the context: payment fields in the form, a compose window, the dialog text. A generic
//!   affirmative ("OK", "Continue", "✓", an icon-only button, Enter) inherits the context's risk,
//!   so a send hidden behind "OK" in a compose window is still a send.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::glob::glob_match;
use crate::text::{Keyword, fold_confusables, normalize, trim_label};
use crate::{ActionContext, ActionDescriptor, RiskClass};

/// The classifier's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    /// Effective risk: never below the declared risk.
    pub risk: RiskClass,
    /// The declared risk, for the record.
    pub declared: RiskClass,
    /// Why the risk was raised (empty when it was not).
    pub reasons: Vec<String>,
}

impl Classification {
    pub fn raised(&self) -> bool {
        self.risk > self.declared
    }
}

/// Table-driven classifier. Construction compiles the keyword tables once; use
/// [`RiskClassifier::global`] to share them.
#[derive(Debug)]
pub struct RiskClassifier {
    label_keywords: Vec<(Keyword, RiskClass)>,
    affirmatives: HashSet<String>,
    dismissives: HashSet<String>,
    payment_fields: Vec<Keyword>,
    compose_fields: Vec<Keyword>,
    compose_titles: Vec<Keyword>,
    secure_labels: Vec<Keyword>,
    card_labels: Vec<Keyword>,
    cancel_phrases: Vec<Keyword>,
    store_labels: Vec<Keyword>,
    currency_codes: Vec<Keyword>,
}

impl Default for RiskClassifier {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerbKind {
    Activate,
    Type,
    Key,
    Navigate,
    Observe,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppKind {
    Settings,
    Terminal,
    AppStore,
    Messaging,
    Mail,
    FileManager,
}

struct Acc<'a> {
    risk: RiskClass,
    reasons: &'a mut Vec<String>,
}

impl Acc<'_> {
    fn raise(&mut self, risk: RiskClass, reason: impl FnOnce() -> String) {
        if risk > RiskClass::Observe && risk >= self.risk {
            if risk > self.risk || self.reasons.is_empty() {
                self.reasons.push(reason());
            }
            self.risk = risk;
        }
    }
}

impl RiskClassifier {
    pub fn new() -> Self {
        let kw = |list: &[&str]| list.iter().map(|k| Keyword::new(k)).collect::<Vec<_>>();
        let mut label_keywords = Vec::new();
        for (class, list) in [
            (RiskClass::Send, SEND_WORDS),
            (RiskClass::Destroy, DESTROY_WORDS),
            (RiskClass::Spend, SPEND_WORDS),
            (RiskClass::Security, SECURITY_WORDS),
        ] {
            label_keywords.extend(list.iter().map(|k| (Keyword::new(k), class)));
        }
        Self {
            label_keywords,
            affirmatives: AFFIRMATIVES.iter().map(|s| normalize(s)).collect(),
            dismissives: DISMISSIVES.iter().map(|s| normalize(s)).collect(),
            payment_fields: kw(PAYMENT_FIELDS),
            compose_fields: kw(COMPOSE_FIELDS),
            compose_titles: kw(COMPOSE_TITLES),
            secure_labels: kw(SECURE_LABELS),
            card_labels: kw(CARD_LABELS),
            cancel_phrases: kw(CANCEL_PHRASES),
            store_labels: kw(STORE_LABELS),
            currency_codes: kw(CURRENCY_CODES),
        }
    }

    /// A shared instance (tables compiled once per process).
    pub fn global() -> &'static RiskClassifier {
        static GLOBAL: OnceLock<RiskClassifier> = OnceLock::new();
        GLOBAL.get_or_init(RiskClassifier::new)
    }

    /// Classify with no context beyond the descriptor.
    pub fn classify(&self, action: &ActionDescriptor) -> Classification {
        self.classify_with_context(action, &ActionContext::default())
    }

    pub fn classify_with_context(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
    ) -> Classification {
        let mut reasons = Vec::new();
        let mut acc = Acc {
            risk: action.declared_risk,
            reasons: &mut reasons,
        };
        let verb = normalize(&action.verb.replace(['_', '-', '.', ':', '/'], " "));
        let kind = verb_kind(&verb);
        let apps = app_kinds(&action.target_app, ctx.url.as_deref());
        let label = action.target_label.as_deref().map(normalize);
        let preview = action.text_preview.as_deref();

        // 1. The verb itself.
        if kind != VerbKind::Key {
            for token in verb.split(' ') {
                if let Some(class) = verb_token_class(token) {
                    acc.raise(class, || {
                        format!("verb “{}” is a {class} verb", action.verb)
                    });
                }
            }
        }

        // 2. The label, for anything that activates or follows it.
        let label_counts = matches!(
            kind,
            VerbKind::Activate | VerbKind::Navigate | VerbKind::Other
        );
        if label_counts && let Some(label) = &label {
            if let Some((class, word)) = self.keyword_risk(label) {
                acc.raise(class, || format!("label matches {class} keyword “{word}”"));
            }
            if self.has_price(label) {
                acc.raise(RiskClass::Spend, || "label shows a price".into());
            }
        }

        // 3. URLs and deep links in the label or typed text (navigation, address bar, links).
        if matches!(
            kind,
            VerbKind::Navigate | VerbKind::Activate | VerbKind::Other
        ) {
            for text in [action.target_label.as_deref(), preview]
                .into_iter()
                .flatten()
            {
                if looks_like_url(text)
                    && let Some((class, word)) = self.keyword_risk(&normalize(text))
                {
                    acc.raise(class, || format!("URL matches {class} keyword “{word}”"));
                }
                if text.trim_start().to_ascii_lowercase().starts_with("tel:") {
                    acc.raise(RiskClass::Send, || "tel: link places a call".into());
                }
            }
        }

        // 4. Context: what a generic affirmative, an icon-only button, a submit or Enter inherits.
        let context = self.context_risk(action, ctx, &apps);
        let label_is_dismissive = label
            .as_deref()
            .is_some_and(|l| self.dismissives.contains(trim_label(l)));
        let generic = label
            .as_deref()
            .is_none_or(|l| trim_label(l).is_empty() || self.affirmatives.contains(trim_label(l)))
            || ctx
                .role
                .as_deref()
                .is_some_and(|r| normalize(r).contains("submit"));
        let dialog_forces = ctx
            .dialog_text
            .as_deref()
            .is_some_and(|d| self.cancel_phrases.iter().any(|k| k.matches(&normalize(d))));
        let inherits = match kind {
            VerbKind::Activate | VerbKind::Other => {
                (generic && !label_is_dismissive)
                    || verb.split(' ').any(|t| t == "submit")
                    || dialog_forces
            }
            _ => false,
        };
        if inherits && let Some((class, why)) = &context {
            acc.raise(*class, || {
                format!("generic control inherits context: {why}")
            });
        }

        // 5. App-specific activation.
        if matches!(kind, VerbKind::Activate | VerbKind::Other) && !label_is_dismissive {
            if apps.contains(&AppKind::Settings) {
                acc.raise(RiskClass::Security, || {
                    "changes a system settings app".into()
                });
            }
            if apps.contains(&AppKind::AppStore)
                && (generic
                    || label
                        .as_deref()
                        .is_some_and(|l| self.store_labels.iter().any(|k| k.matches(l))))
            {
                acc.raise(RiskClass::Security, || "installs from an app store".into());
            }
        }

        // 6. Key presses.
        if kind == VerbKind::Key {
            let keys = [action.target_label.as_deref(), preview]
                .into_iter()
                .flatten()
                .map(normalize)
                .collect::<Vec<_>>()
                .join(" ");
            let k = parse_keys(&keys);
            let terminal = apps.contains(&AppKind::Terminal);
            if k.ctrl_o && terminal {
                acc.raise(RiskClass::Security, || {
                    "Ctrl+O in a terminal runs the current command".into()
                });
            }
            if k.enter {
                if k.cmd_or_ctrl {
                    acc.raise(RiskClass::Send, || "Ctrl/Cmd+Enter sends".into());
                }
                if apps.contains(&AppKind::Terminal) {
                    acc.raise(RiskClass::Security, || {
                        "Enter in a terminal runs a command".into()
                    });
                }
                if apps.contains(&AppKind::Messaging) {
                    acc.raise(RiskClass::Send, || "Enter in a chat app sends".into());
                }
                if let Some((class, why)) = &context {
                    acc.raise(*class, || format!("Enter submits: {why}"));
                }
            }
            if k.delete && (k.any_modifier || apps.contains(&AppKind::FileManager)) {
                acc.raise(RiskClass::Destroy, || "delete shortcut".into());
            }
        }

        // 7. Typing.
        if kind == VerbKind::Type {
            let label_secure = label
                .as_deref()
                .is_some_and(|l| self.secure_labels.iter().any(|k| k.matches(l)));
            if ctx.secure_field || label_secure {
                acc.raise(RiskClass::Security, || "types into a secure field".into());
            }
            let card_field = label
                .as_deref()
                .is_some_and(|l| self.card_labels.iter().any(|k| k.matches(l)));
            if card_field || preview.is_some_and(contains_card_number) {
                acc.raise(RiskClass::Spend, || "enters payment card details".into());
            }
            if let Some(p) = preview {
                // A line break anywhere submits what comes before it (a terminal runs each line,
                // a chat sends it), not only a trailing one.
                let submits = p.contains(['\n', '\r', '\u{2028}', '\u{2029}', '\u{85}']);
                if submits && apps.contains(&AppKind::Messaging) {
                    acc.raise(RiskClass::Send, || "newline in a chat app sends".into());
                }
                if submits && apps.contains(&AppKind::Terminal) {
                    acc.raise(RiskClass::Security, || {
                        "newline in a terminal runs a command".into()
                    });
                }
                if submits && let Some((class, why)) = &context {
                    acc.raise(*class, || format!("newline submits: {why}"));
                }
            }
        }

        // 8. Shell commands, wherever they are typed or run.
        let shellish = apps.contains(&AppKind::Terminal)
            || verb
                .split(' ')
                .any(|t| matches!(t, "shell" | "exec" | "execute" | "command" | "run"));
        if shellish && let Some(p) = preview {
            let p = normalize(p);
            if let Some(pat) = SHELL_DANGER.iter().find(|pat| p.contains(*pat)) {
                acc.raise(RiskClass::Security, || {
                    format!("dangerous shell command “{pat}”")
                });
            }
        }

        let risk = acc.risk;
        Classification {
            risk,
            declared: action.declared_risk,
            reasons,
        }
    }

    /// Highest-risk keyword in `text` (already normalized), checking the confusable-folded form
    /// too.
    fn keyword_risk(&self, text: &str) -> Option<(RiskClass, String)> {
        let folded = fold_confusables(text);
        let mut best: Option<(RiskClass, String)> = None;
        for (kw, class) in &self.label_keywords {
            if best.as_ref().is_some_and(|(b, _)| *b >= *class) {
                continue;
            }
            if kw.matches(text) || (folded != text && kw.matches(&folded)) {
                best = Some((*class, kw.as_str().to_string()));
            }
        }
        best
    }

    fn has_price(&self, label: &str) -> bool {
        label.chars().any(|c| c.is_ascii_digit())
            && (label.chars().any(|c| CURRENCY_SYMBOLS.contains(&c))
                || self.currency_codes.iter().any(|k| k.matches(label)))
    }

    /// The risk the surroundings imply for a generic "OK"/Enter, with a reason.
    fn context_risk(
        &self,
        action: &ActionDescriptor,
        ctx: &ActionContext,
        apps: &[AppKind],
    ) -> Option<(RiskClass, String)> {
        let mut best: Option<(RiskClass, String)> = None;
        let mut take = |class: RiskClass, why: String| {
            if best.as_ref().is_none_or(|(b, _)| class > *b) {
                best = Some((class, why));
            }
        };
        let fields: Vec<String> = ctx.form_fields.iter().map(|f| normalize(f)).collect();
        if fields
            .iter()
            .any(|f| self.payment_fields.iter().any(|k| k.matches(f)))
        {
            take(RiskClass::Spend, "the form has payment fields".into());
        }
        if fields.iter().any(|f| {
            matches!(
                trim_label(f),
                "to" | "cc" | "bcc" | "an" | "à" | "para" | "aan"
            ) || self.compose_fields.iter().any(|k| k.matches(f))
        }) {
            take(RiskClass::Send, "the form is a message compose form".into());
        }
        if let Some(title) = &ctx.window_title {
            let title = normalize(title);
            if self.compose_titles.iter().any(|k| k.matches(&title)) {
                take(RiskClass::Send, "the window is a compose window".into());
            }
            if let Some((class, word)) = self.keyword_risk(&title) {
                take(class, format!("the window title mentions “{word}”"));
            }
        }
        if let Some(dialog) = &ctx.dialog_text
            && let Some((class, word)) = self.keyword_risk(&normalize(dialog))
        {
            take(class, format!("the dialog mentions “{word}”"));
        }
        if let Some(url) = &ctx.url {
            let url = normalize(url);
            if let Some((class, word)) = self.keyword_risk(&url) {
                take(class, format!("the page URL mentions “{word}”"));
            }
            if url.contains("compose") {
                take(RiskClass::Send, "the page is a compose page".into());
            }
        }
        if apps.contains(&AppKind::Messaging) {
            take(RiskClass::Send, "the app is a chat app".into());
        }
        let _ = action;
        best
    }
}

fn verb_kind(verb: &str) -> VerbKind {
    let compact = verb.replace(' ', "");
    match compact.as_str() {
        "sendkeys" | "key" | "keys" | "keypress" | "presskey" | "hotkey" | "shortcut"
        | "keycombo" | "keychord" | "keydown" | "keyup" => return VerbKind::Key,
        "type" | "typetext" | "setvalue" | "fill" | "input" | "entertext" | "paste" | "settext"
        | "inserttext" | "write" | "writetext" => return VerbKind::Type,
        "openurl" | "navigate" | "goto" | "open" | "launch" | "openapp" | "visit" | "load"
        | "browse" | "followlink" | "deeplink" | "openlink" | "openfile" => {
            return VerbKind::Navigate;
        }
        "scroll" | "hover" | "focus" | "read" | "screenshot" | "get" | "find" | "wait"
        | "inspect" | "observe" | "select" | "move" | "mousemove" | "zoom" => {
            return VerbKind::Observe;
        }
        _ => {}
    }
    let first = verb.split(' ').next().unwrap_or("");
    match first {
        "click" | "doubleclick" | "double" | "tap" | "press" | "activate" | "invoke" | "submit"
        | "perform" | "default" | "menu" | "choose" | "toggle" | "check" | "confirm" | "accept"
        | "right" | "triple" | "long" => VerbKind::Activate,
        "type" | "set" | "fill" | "enter" | "insert" => VerbKind::Type,
        "open" | "navigate" | "go" | "launch" | "visit" => VerbKind::Navigate,
        "scroll" | "hover" | "focus" | "read" | "get" | "find" | "list" | "wait" => {
            VerbKind::Observe
        }
        _ => VerbKind::Other,
    }
}

fn verb_token_class(token: &str) -> Option<RiskClass> {
    Some(match token {
        "send" | "submit" | "post" | "publish" | "reply" | "forward" | "share" | "email"
        | "mail" | "sms" | "tweet" | "upload" | "invite" | "comment" | "notify" | "broadcast"
        | "approve" | "call" | "dial" | "message" => RiskClass::Send,
        "delete" | "del" | "remove" | "rm" | "rmdir" | "unlink" | "trash" | "erase" | "wipe"
        | "destroy" | "purge" | "discard" | "archive" | "overwrite" | "truncate"
        | "unsubscribe" | "empty" | "clear" | "drop" | "reset" | "cancel" | "kill"
        | "terminate" | "deactivate" | "unsend" => RiskClass::Destroy,
        "pay" | "payment" | "purchase" | "buy" | "order" | "checkout" | "book" | "reserve"
        | "transfer" | "subscribe" | "donate" | "sign" | "trade" | "sell" | "bid" | "tip"
        | "wire" | "withdraw" | "invest" | "charge" | "rent" => RiskClass::Spend,
        "install" | "uninstall" | "grant" | "authorize" | "authorise" | "permit" | "chmod"
        | "chown" | "sudo" | "exec" | "execute" | "shell" | "command" | "cmd" | "terminal"
        | "script" | "powershell" | "password" | "credential" | "credentials" | "keychain"
        | "permission" | "permissions" | "revoke" | "firewall" | "admin" | "elevate" => {
            RiskClass::Security
        }
        _ => return None,
    })
}

fn app_kinds(app: &str, url: Option<&str>) -> Vec<AppKind> {
    let mut targets = vec![app.to_lowercase()];
    if let Some(host) = url.and_then(crate::rules::host_of) {
        targets.push(host);
    }
    if let Some(host) = crate::rules::host_of(app) {
        targets.push(host);
    }
    let mut kinds = Vec::new();
    for (kind, globs) in [
        (AppKind::Settings, SETTINGS_APPS),
        (AppKind::Terminal, TERMINAL_APPS),
        (AppKind::AppStore, STORE_APPS),
        (AppKind::Messaging, MESSAGING_APPS),
        (AppKind::Mail, MAIL_APPS),
        (AppKind::FileManager, FILE_MANAGERS),
    ] {
        if targets
            .iter()
            .any(|t| globs.iter().any(|g| glob_match(g, t)))
        {
            kinds.push(kind);
        }
    }
    kinds
}

fn looks_like_url(s: &str) -> bool {
    let s = s.trim();
    s.contains("://")
        || s.starts_with("www.")
        || s.split_once(':').is_some_and(|(scheme, rest)| {
            !scheme.is_empty()
                && !rest.starts_with(' ')
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                && scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
        })
}

const CURRENCY_SYMBOLS: &[char] = &[
    '$', '€', '£', '¥', '₹', '₩', '₽', '₺', '₪', '฿', '₫', '₴', '₦', '₱', '₡', '₲', '₵', '₸', '₼',
    '₾', '﷼', '¢',
];
const CURRENCY_CODES: &[&str] = &[
    "usd", "eur", "gbp", "jpy", "cny", "rmb", "inr", "chf", "cad", "aud", "brl", "rub", "krw",
    "mxn", "sek", "nok", "dkk", "pln", "zł", "kr", "btc", "eth", "usdc", "usdt", "元", "円", "원",
    "руб",
];

struct Keys {
    /// Enter, or a control chord terminals and many inputs treat as Enter (Ctrl/Cmd+J, +M).
    enter: bool,
    /// Ctrl/Cmd+O (readline's operate-and-get-next: runs the line in a terminal).
    ctrl_o: bool,
    delete: bool,
    cmd_or_ctrl: bool,
    any_modifier: bool,
}

fn parse_keys(keys: &str) -> Keys {
    let tokens: Vec<&str> = keys
        .split(|c: char| c == '+' || c == '-' || c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    let has = |set: &[&str]| tokens.iter().any(|t| set.contains(t));
    let sym = |set: &[char]| keys.chars().any(|c| set.contains(&c));
    let delete = has(&["delete", "del", "backspace", "forwarddelete"]) || sym(&['⌫', '⌦']);
    let cmd_or_ctrl = has(&[
        "cmd", "command", "ctrl", "control", "meta", "super", "win", "primary", "mod", "strg",
    ]) || sym(&['⌘', '⌃', '^']);
    let enter = has(&["enter", "return", "kp_enter", "numpadenter", "ret", "\\n"])
        || sym(&[
            '↩', '⏎', '↵', '⌤', '\n', '\r', '\u{2028}', '\u{2029}', '\u{85}',
        ])
        || (cmd_or_ctrl && has(&["j", "m"]));
    let ctrl_o = cmd_or_ctrl && has(&["o"]);
    let any_modifier =
        cmd_or_ctrl || has(&["shift", "alt", "option", "opt", "altgr"]) || sym(&['⇧', '⌥']);
    Keys {
        enter,
        ctrl_o,
        delete,
        cmd_or_ctrl,
        any_modifier,
    }
}

/// A Luhn-valid run of 13–19 digits (spaces and dashes allowed between them).
pub(crate) fn contains_card_number(text: &str) -> bool {
    card_spans(text).next().is_some()
}

/// Byte ranges of Luhn-valid card numbers in `text`.
pub(crate) fn card_spans(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let bytes = text.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                i += 1;
                continue;
            }
            let start = i;
            let mut digits = Vec::new();
            let mut end = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || bytes[i] == b' ' || bytes[i] == b'-')
            {
                if bytes[i].is_ascii_digit() {
                    digits.push(bytes[i] - b'0');
                    end = i + 1;
                }
                i += 1;
            }
            if (13..=19).contains(&digits.len()) && luhn(&digits) {
                return Some((start, end));
            }
        }
        None
    })
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let d = u32::from(*d);
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

// ---------------------------------------------------------------------------------------------
// Tables. `word` = whole word, `stem*` = word prefix, phrases match at word boundaries; CJK,
// Arabic and Devanagari entries match as substrings.
// ---------------------------------------------------------------------------------------------

const SEND_WORDS: &[&str] = &[
    // English
    "send",
    "sending",
    "submit*",
    "post",
    "post now",
    "publish*",
    "send reply",
    "post reply",
    "reply all",
    "forward message",
    "forward email",
    "forward to",
    "share",
    "tweet",
    "upload",
    "invite",
    "broadcast",
    "approve",
    "notify",
    "resend",
    "send money",
    "comment",
    "place call",
    "call now",
    // Spanish / Portuguese
    "enviar*",
    "envía",
    "envia",
    "publicar",
    "responder",
    "reenviar",
    "compartir",
    "compartilhar",
    "partilhar",
    "encaminhar",
    // French
    "envoyer",
    "envoi",
    "publier",
    "répondre",
    "transférer",
    "partager",
    "soumettre",
    "valider",
    // German
    "senden",
    "absenden",
    "abschicken",
    "verschicken",
    "schicken",
    "veröffentlichen",
    "antworten",
    "weiterleiten",
    "teilen",
    "einreichen",
    // Italian
    "invia*",
    "pubblica",
    "rispondi",
    "inoltra",
    "condividi",
    // Dutch
    "verzenden",
    "versturen",
    "verstuur",
    "verzend",
    "delen",
    "plaatsen",
    "beantwoorden",
    "doorsturen",
    // Polish / Turkish
    "wyślij",
    "wysłać",
    "opublikuj",
    "udostępnij",
    "odpowiedz",
    "gönder*",
    "paylaş",
    "yayınla",
    "yanıtla",
    // Russian
    "отправ*",
    "послать",
    "опубликовать",
    "ответить",
    "переслать",
    "поделиться",
    // CJK
    "发送",
    "發送",
    "提交",
    "发布",
    "發佈",
    "發布",
    "回复",
    "回覆",
    "转发",
    "轉寄",
    "分享",
    "寄出",
    "送信",
    "投稿",
    "返信",
    "転送",
    "共有",
    "提出",
    "보내기",
    "전송",
    "제출",
    "게시",
    "답장",
    "공유",
    // Arabic / Hindi
    "إرسال",
    "ارسال",
    "أرسل",
    "نشر",
    "مشاركة",
    "भेजें",
    "भेजना",
    "भेजे",
    "प्रकाशित",
    "साझा",
    "जमा करें",
];

const DESTROY_WORDS: &[&str] = &[
    // English
    "delet*",
    "remov*",
    "erase*",
    "trash it",
    "trash this",
    "move to trash",
    "move to bin",
    "send to trash",
    "bin it",
    "wipe*",
    "purge",
    "destroy*",
    "discard*",
    "clear all",
    "clear history",
    "clear data",
    "clear browsing data",
    "empty trash",
    "empty the trash",
    "empty bin",
    "empty the bin",
    "empty recycle bin",
    "empty the recycle bin",
    "overwrite*",
    "replace",
    "replace all",
    "format disk",
    "format drive",
    "erase disk",
    "unsubscrib*",
    "opt out",
    "cancel subscription",
    "cancel membership",
    "cancel plan",
    "cancel order",
    "cancel account",
    "cancel booking",
    "cancel reservation",
    "cancel my",
    "cancel your",
    "close account",
    "deactivat*",
    "terminate",
    "archive",
    "factory reset",
    "reset",
    "unsend",
    "leave group",
    "drop table",
    "permanently",
    // Spanish / Portuguese
    "eliminar*",
    "borrar*",
    "suprimir",
    "quitar",
    "descartar",
    "mover a la papelera",
    "vaciar",
    "cancelar suscripción",
    "darse de baja",
    "darme de baja",
    "excluir",
    "apagar",
    "remover",
    "mover para a lixeira",
    "cancelar assinatura",
    "descadastrar",
    // French
    "supprim*",
    "effacer",
    "retirer",
    "mettre à la corbeille",
    "se désabonner",
    "désabonner",
    "se désinscrire",
    "désinscri*",
    "résilier",
    "vider",
    // German
    "lösch*",
    "entfern*",
    "in den papierkorb",
    "verwerfen",
    "kündig*",
    "abbestellen",
    "abmelden",
    "leeren",
    "endgültig",
    // Italian (`cancella` means delete)
    "elimina*",
    "cancella*",
    "rimuovi",
    "sposta nel cestino",
    "annulla iscrizione",
    "disiscriviti",
    // Dutch
    "verwijder*",
    "wissen",
    "naar prullenbak",
    "opzeggen",
    "uitschrijven",
    "afmelden",
    // Polish / Turkish
    "usuń",
    "usunąć",
    "wymaż",
    "przenieś do kosza",
    "wypisz*",
    "sil",
    "kaldır",
    "çöpe taşı",
    "abonelikten çık",
    // Russian
    "удал*",
    "стереть",
    "очистить",
    "отписаться",
    "в корзину",
    // CJK
    "删除",
    "刪除",
    "移除",
    "清空",
    "取消订阅",
    "取消訂閱",
    "退订",
    "退訂",
    "移到废纸篓",
    "移到回收站",
    "削除",
    "消去",
    "ゴミ箱に入れる",
    "購読解除",
    "登録解除",
    "解約",
    "삭제",
    "제거",
    "휴지통으로 이동",
    "구독 취소",
    "구독취소",
    "해지",
    // Arabic / Hindi
    "حذف",
    "إزالة",
    "ازالة",
    "مسح",
    "إلغاء الاشتراك",
    "الغاء الاشتراك",
    "हटाएं",
    "हटाएँ",
    "मिटाएं",
    "डिलीट",
    "सदस्यता रद्द",
];

const SPEND_WORDS: &[&str] = &[
    // English
    "pay",
    "pay now",
    "payment*",
    "purchas*",
    "buy*",
    "checkout",
    "check out",
    "place order",
    "order",
    "order now",
    "complete order",
    "submit order",
    "subscribe",
    "subscribe now",
    "donate*",
    "transfer*",
    "send money",
    "wire",
    "withdraw*",
    "book now",
    "book it",
    "book this",
    "book a",
    "book your",
    "reserve",
    "reservation",
    "e-sign",
    "sign contract",
    "sign document",
    "sign here",
    "sign and submit",
    "upgrade",
    "renew*",
    "start trial",
    "start free trial",
    "bid",
    "place bid",
    "trade",
    "sell",
    "invest",
    "rent",
    "1-click",
    "confirm and pay",
    "add funds",
    "top up",
    "authorize payment",
    "complete purchase",
    "redeem",
    "paypal",
    "apple pay",
    "google pay",
    // Spanish / Portuguese
    "pagar*",
    "pago",
    "comprar*",
    "compra",
    "realizar pedido",
    "pedido",
    "suscribirse",
    "suscribir",
    "suscríbete",
    "donar",
    "transferir",
    "transferencia",
    "transferência",
    "reservar",
    "finalizar compra",
    "assinar",
    "pagamento",
    // French
    "payer",
    "paiement",
    "acheter",
    "achat",
    "commander",
    "commande",
    "s'abonner",
    "abonner",
    "virement",
    "réserver",
    "signer",
    // German
    "bezahl*",
    "zahlen",
    "zahlung*",
    "kaufen",
    "jetzt kaufen",
    "kauf*",
    "bestell*",
    "kostenpflichtig*",
    "zahlungspflichtig*",
    "abonnier*",
    "überweis*",
    "buchen",
    "unterschreiben",
    // Italian
    "paga*",
    "acquist*",
    "ordina",
    "abbonati",
    "bonifico",
    "prenota",
    // Dutch
    "betalen",
    "betaal*",
    "kopen",
    "koop",
    "bestellen",
    "afrekenen",
    "abonneren",
    "overmaken",
    "reserveren",
    // Polish / Turkish
    "zapłać",
    "kup",
    "kupuję",
    "zamów",
    "zamawiam",
    "przelew",
    "subskrybuj",
    "öde*",
    "satın al",
    "sipariş*",
    "abone ol",
    "havale",
    // Russian
    "оплат*",
    "заплатить",
    "купить",
    "заказать",
    "оформить заказ",
    "подписаться",
    "перевести",
    "перевод",
    // CJK
    "支付",
    "付款",
    "购买",
    "購買",
    "立即购买",
    "下单",
    "下單",
    "结账",
    "結帳",
    "结算",
    "订阅",
    "訂閱",
    "转账",
    "轉帳",
    "预订",
    "預訂",
    "充值",
    "支払",
    "購入",
    "注文",
    "決済",
    "購読",
    "振込",
    "予約",
    "결제",
    "구매",
    "주문",
    "구독",
    "송금",
    "이체",
    "예약",
    // Arabic / Hindi
    "ادفع",
    "دفع",
    "شراء",
    "اشتر",
    "اطلب",
    "تحويل",
    "اشتراك",
    "भुगतान",
    "खरीदें",
    "ख़रीदें",
    "ऑर्डर",
    "सदस्यता लें",
    "ट्रांसफर",
];

const SECURITY_WORDS: &[&str] = &[
    // English
    "install*",
    "uninstall*",
    "revoke*",
    "grant*",
    "allow",
    "always allow",
    "allow access",
    "authoriz*",
    "authoris*",
    "permission*",
    "trust",
    "trust this",
    "password*",
    "passcode",
    "2fa",
    "two-factor",
    "mfa",
    "security",
    "sudo",
    "admin*",
    "administrator",
    "root",
    "firewall",
    "antivirus",
    "disable protection",
    "developer mode",
    "api key",
    "access token",
    "certificate",
    "keychain",
    "login items",
    "full disk access",
    "screen recording",
    "make admin",
    "enable macros",
    "run anyway",
    "open anyway",
    "elevate",
    "transfer ownership",
    // Spanish / Portuguese
    "instalar",
    "permitir",
    "autorizar",
    "contraseña",
    "conceder",
    "senha",
    // French
    "installer",
    "autoriser",
    "accorder",
    "mot de passe",
    // German
    "installier*",
    "erlauben",
    "zulassen",
    "autorisieren",
    "passwort",
    "kennwort",
    "berechtigung*",
    // Italian
    "installa",
    "consenti",
    "autorizza",
    // Dutch
    "installeren",
    "toestaan",
    "wachtwoord",
    "machtigen",
    // Polish / Turkish
    "zainstaluj",
    "zezwól",
    "hasło",
    "yükle",
    "izin ver",
    "şifre",
    "parola",
    // Russian
    "установить",
    "разрешить",
    "пароль",
    "авторизовать",
    // CJK
    "安装",
    "安裝",
    "允许",
    "允許",
    "授权",
    "授權",
    "密码",
    "密碼",
    "インストール",
    "許可",
    "パスワード",
    "설치",
    "허용",
    "승인",
    "비밀번호",
    "암호",
    // Arabic / Hindi
    "تثبيت",
    "السماح",
    "كلمة المرور",
    "इंस्टॉल",
    "अनुमति",
    "पासवर्ड",
];

/// Labels that mean "go ahead with whatever this dialog or form is for".
const AFFIRMATIVES: &[&str] = &[
    "ok",
    "okay",
    "yes",
    "y",
    "continue",
    "confirm",
    "done",
    "next",
    "proceed",
    "accept",
    "agree",
    "i agree",
    "go",
    "go ahead",
    "apply",
    "save",
    "finish",
    "complete",
    "got it",
    "sure",
    "yes, continue",
    "submit",
    "enter",
    "✓",
    "✔",
    "→",
    "➤",
    "▶",
    "⏎",
    "↵",
    "»",
    "sí",
    "si",
    "aceptar",
    "continuar",
    "confirmar",
    "siguiente",
    "listo",
    "vale",
    "oui",
    "continuer",
    "confirmer",
    "suivant",
    "d'accord",
    "terminé",
    "ja",
    "weiter",
    "bestätigen",
    "fertig",
    "fortfahren",
    "continua",
    "conferma",
    "avanti",
    "sim",
    "próximo",
    "seguinte",
    "doorgaan",
    "bevestigen",
    "volgende",
    "да",
    "продолжить",
    "подтвердить",
    "далее",
    "готово",
    "确定",
    "確定",
    "确认",
    "確認",
    "继续",
    "繼續",
    "好",
    "是",
    "下一步",
    "完成",
    "はい",
    "続ける",
    "次へ",
    "完了",
    "확인",
    "예",
    "계속",
    "다음",
    "완료",
    "نعم",
    "موافق",
    "متابعة",
    "تأكيد",
    "हाँ",
    "ठीक है",
    "जारी रखें",
    "पुष्टि करें",
    "evet",
    "tamam",
    "devam",
    "onayla",
    "tak",
    "dalej",
    "kontynuuj",
    "potwierdź",
];

/// Labels that back out; they do not inherit the context's risk.
const DISMISSIVES: &[&str] = &[
    "cancel",
    "no",
    "close",
    "dismiss",
    "not now",
    "back",
    "later",
    "skip",
    "keep",
    "never",
    "don't allow",
    "deny",
    "block",
    "x",
    "×",
    "✕",
    "cancelar",
    "annuler",
    "abbrechen",
    "annulla",
    "annuleren",
    "отмена",
    "取消",
    "キャンセル",
    "취소",
    "إلغاء",
    "रद्द करें",
    "iptal",
    "anuluj",
    "non",
    "nein",
    "nee",
    "нет",
    "いいえ",
    "아니요",
    "否",
];

const PAYMENT_FIELDS: &[&str] = &[
    "card number",
    "cardnumber",
    "cc-number",
    "cc number",
    "cc-csc",
    "cc-exp*",
    "cvv",
    "cvc",
    "csc",
    "security code",
    "expiry*",
    "expiration*",
    "name on card",
    "iban",
    "routing number",
    "account number",
    "swift",
    "bic",
    "billing address",
    "credit card",
    "debit card",
    "amount",
    "número de tarjeta",
    "tarjeta",
    "numéro de carte",
    "carte bancaire",
    "kartennummer",
    "kreditkarte",
    "numero carta",
    "número do cartão",
    "kaartnummer",
    "номер карты",
    "卡号",
    "卡號",
    "カード番号",
    "카드번호",
    "upi",
    "paypal",
];

const COMPOSE_FIELDS: &[&str] = &[
    "subject",
    "recipient*",
    "compose",
    "new message",
    "add recipients",
    "betreff",
    "objet",
    "asunto",
    "oggetto",
    "assunto",
    "onderwerp",
    "temat",
    "konu",
    "тема",
    "件名",
    "主题",
    "主旨",
    "제목",
    "message body",
    "write a message",
    "type a message",
];

const COMPOSE_TITLES: &[&str] = &[
    "new message",
    "compose",
    "new email",
    "new mail",
    "reply",
    "re:",
    "fwd:",
    "fw:",
    "forward",
    "draft",
    "neue e-mail",
    "neue nachricht",
    "nouveau message",
    "nuevo mensaje",
    "nuevo correo",
    "nuova email",
    "nuovo messaggio",
    "nieuw bericht",
    "nova mensagem",
    "новое письмо",
    "новое сообщение",
    "新邮件",
    "写信",
    "写邮件",
    "新規メッセージ",
    "新規作成",
    "새 메시지",
    "편지쓰기",
];

const SECURE_LABELS: &[&str] = &[
    "password*",
    "passcode",
    "passphrase",
    "pin",
    "pin code",
    "otp",
    "one-time code",
    "verification code",
    "2fa",
    "security code",
    "secret",
    "api key",
    "token",
    "ssn",
    "social security",
    "contraseña",
    "senha",
    "mot de passe",
    "passwort",
    "kennwort",
    "wachtwoord",
    "hasło",
    "şifre",
    "parola",
    "пароль",
    "密码",
    "密碼",
    "パスワード",
    "暗証番号",
    "비밀번호",
    "암호",
    "كلمة المرور",
    "पासवर्ड",
];

const CARD_LABELS: &[&str] = &[
    "card number",
    "cardnumber",
    "cc-number",
    "cvv",
    "cvc",
    "csc",
    "credit card",
    "debit card",
    "iban",
    "account number",
    "routing number",
    "tarjeta",
    "numéro de carte",
    "kartennummer",
    "номер карты",
    "卡号",
    "カード番号",
    "카드번호",
];

/// Dialog text that makes even a "Cancel" button dangerous.
const CANCEL_PHRASES: &[&str] = &[
    "cancel your subscription",
    "cancel subscription",
    "cancel your membership",
    "cancel your plan",
    "cancel your order",
    "cancel your account",
    "cancel your booking",
    "cancel your reservation",
];

const STORE_LABELS: &[&str] = &["get", "install*", "update", "download", "obtain", "buy*"];

const SHELL_DANGER: &[&str] = &[
    "rm -rf",
    "rm -fr",
    "rm -r ",
    "sudo ",
    "mkfs",
    "dd if=",
    ":(){",
    "chmod 777",
    "chmod -r",
    "chown ",
    "| sh",
    "|sh",
    "| bash",
    "|bash",
    "| zsh",
    "curl ",
    "wget ",
    "format c:",
    "del /f",
    "del /s",
    "rd /s",
    "rmdir /s",
    "remove-item",
    "set-executionpolicy",
    "reg delete",
    "reg add",
    "diskpart",
    "shutdown",
    "killall",
    "launchctl",
    "systemctl",
    "csrutil",
    "spctl",
    "defaults write",
    "security delete",
    "security add",
    "ssh-keygen",
    "push --force",
    "push -f",
    "reset --hard",
    "drop table",
    "drop database",
    "truncate table",
    "delete from",
    "iex(",
    "invoke-expression",
    "bcdedit",
    "vssadmin",
    "cipher /w",
];

const SETTINGS_APPS: &[&str] = &[
    "com.apple.systempreferences",
    "com.apple.settings*",
    "system settings",
    "system preferences",
    "ms-settings:*",
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

const TERMINAL_APPS: &[&str] = &[
    "com.apple.terminal",
    "*iterm*",
    "*terminal*",
    "*alacritty*",
    "*kitty*",
    "*wezterm*",
    "*konsole*",
    "cmd.exe",
    "cmd",
    "powershell*",
    "pwsh*",
    "*ghostty*",
    "dev.warp*",
    "*xterm*",
    "*tilix*",
    "*conhost*",
];

const STORE_APPS: &[&str] = &[
    "com.apple.appstore",
    "apps.apple.com",
    "ms-windows-store*",
    "*windowsstore*",
    "com.android.vending",
    "play.google.com",
    "*snap-store*",
    "org.gnome.software*",
    "org.kde.discover*",
    "chromewebstore.google.com",
    "chrome.google.com",
    "addons.mozilla.org",
    "microsoftedge.microsoft.com",
];

const MESSAGING_APPS: &[&str] = &[
    "*slack*",
    "*discord*",
    "com.microsoft.teams*",
    "*msteams*",
    "teams.microsoft.com",
    "teams.live.com",
    "*whatsapp*",
    "*telegram*",
    "org.whispersystems.signal*",
    "*signal-desktop*",
    "com.apple.mobilesms",
    "com.apple.ichat",
    "*messenger*",
    "*wechat*",
    "*weixin*",
    "jp.naver.line*",
    "*.line.me",
    "im.riot*",
    "app.element.io",
    "*mattermost*",
    "*zulip*",
    "*skype*",
    "chat.google.com",
    "web.whatsapp.com",
    "web.telegram.org",
];

const MAIL_APPS: &[&str] = &[
    "com.apple.mail",
    "*outlook*",
    "*thunderbird*",
    "com.readdle.smartemail*",
    "mail.google.com",
    "*gmail*",
    "*protonmail*",
    "mail.proton.me",
    "*fastmail*",
    "mail.yahoo.com",
    "*superhuman*",
    "app.hey.com",
    "org.gnome.evolution*",
    "*mailspring*",
];

const FILE_MANAGERS: &[&str] = &[
    "com.apple.finder",
    "explorer.exe",
    "explorer",
    "*nautilus*",
    "*dolphin*",
    "*thunar*",
    "*nemo*",
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

    // Security review PA-2 (regression).
    #[test]
    fn pa2_line_break_anywhere_submits_and_enter_chords() {
        assert!(
            risk(
                "com.apple.Terminal",
                "type",
                Some("shell"),
                Some("echo ok\nopen -a Calculator ")
            ) >= RiskClass::Security
        );
        assert!(
            risk(
                "com.tinyspeck.slackmacgap",
                "type",
                Some("Message #general"),
                Some("secret stuff\n ")
            ) >= RiskClass::Send
        );
        for sep in ['\r', '\u{2028}', '\u{2029}', '\u{85}'] {
            let text = format!("echo ok{sep}ls ");
            assert!(
                risk("com.apple.Terminal", "type", Some("shell"), Some(&text))
                    >= RiskClass::Security,
                "{sep:?}"
            );
        }
        for k in ["ctrl+j", "ctrl+m", "ctrl+o"] {
            assert!(
                risk("com.apple.Terminal", "key", Some(k), None) >= RiskClass::Security,
                "{k}"
            );
        }
        assert!(risk("com.tinyspeck.slackmacgap", "key", Some("ctrl+j"), None) >= RiskClass::Send);
        // Plain letters and Ctrl+O outside a terminal are not Enter.
        assert!(risk("com.apple.Terminal", "key", Some("j"), None) < RiskClass::Send);
        assert!(risk("com.apple.TextEdit", "key", Some("ctrl+o"), None) < RiskClass::Send);
        assert!(
            risk("com.apple.Terminal", "type", Some("shell"), Some("echo ok")) < RiskClass::Send
        );
    }

    fn risk(app: &str, verb: &str, label: Option<&str>, preview: Option<&str>) -> RiskClass {
        RiskClassifier::global()
            .classify(&action(app, verb, label, preview))
            .risk
    }

    fn risk_ctx(
        app: &str,
        verb: &str,
        label: Option<&str>,
        preview: Option<&str>,
        ctx: &ActionContext,
    ) -> RiskClass {
        RiskClassifier::global()
            .classify_with_context(&action(app, verb, label, preview), ctx)
            .risk
    }

    #[test]
    fn classifier_table() {
        use RiskClass::*;
        type Row = (
            &'static str,
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
            RiskClass,
        );
        let table: &[Row] = &[
            // Benign actions stay where they were declared.
            ("com.apple.mail", "scroll", None, None, Observe),
            ("com.apple.mail", "click", Some("Inbox"), None, Observe),
            ("com.apple.textedit", "click", Some("Bold"), None, Observe),
            ("example.com", "click", Some("Read more"), None, Observe),
            (
                "example.com",
                "click",
                Some("Sender details"),
                None,
                Observe,
            ),
            ("example.com", "click", Some("Bookmark"), None, Observe),
            (
                "example.com",
                "click",
                Some("Display settings"),
                None,
                Observe,
            ),
            ("example.com", "click", Some("Cancel"), None, Observe),
            (
                "example.com",
                "type",
                Some("Search"),
                Some("delete my account"),
                Observe,
            ),
            ("com.apple.finder", "key", Some("Down"), None, Observe),
            // Verbs.
            ("x", "send_email", None, None, Send),
            ("x", "submit", Some("Save"), None, Send),
            ("x", "delete_file", None, None, Destroy),
            ("x", "transfer_funds", None, None, Spend),
            ("x", "install_package", None, None, Security),
            ("x", "send_keys", Some("a"), None, Observe),
            // English labels.
            ("x", "click", Some("Send"), None, Send),
            ("x", "click", Some("Reply all"), None, Send),
            ("x", "click", Some("Delete"), None, Destroy),
            ("x", "click", Some("Move to Trash"), None, Destroy),
            ("x", "click", Some("Unsubscribe"), None, Destroy),
            ("x", "click", Some("Cancel subscription"), None, Destroy),
            ("x", "click", Some("Pay now"), None, Spend),
            ("x", "click", Some("Buy with 1-Click"), None, Spend),
            ("x", "click", Some("Place your order"), None, Spend),
            ("x", "click", Some("Install"), None, Security),
            ("x", "click", Some("Grant access"), None, Security),
            // Other languages.
            ("x", "click", Some("Enviar"), None, Send),
            ("x", "click", Some("Absenden"), None, Send),
            ("x", "click", Some("送信"), None, Send),
            ("x", "click", Some("Löschen"), None, Destroy),
            ("x", "click", Some("Supprimer"), None, Destroy),
            ("x", "click", Some("Удалить"), None, Destroy),
            ("x", "click", Some("永久删除"), None, Destroy),
            ("x", "click", Some("삭제"), None, Destroy),
            ("x", "click", Some("حذف"), None, Destroy),
            ("x", "click", Some("Jetzt kaufen"), None, Spend),
            (
                "x",
                "click",
                Some("Zahlungspflichtig bestellen"),
                None,
                Spend,
            ),
            ("x", "click", Some("立即购买"), None, Spend),
            ("x", "click", Some("Оплатить"), None, Spend),
            ("x", "click", Some("결제하기"), None, Spend),
            ("x", "click", Some("Installieren"), None, Security),
            // Obfuscation.
            ("x", "click", Some("De\u{200B}lete"), None, Destroy),
            ("x", "click", Some("ＤＥＬＥＴＥ"), None, Destroy),
            ("x", "click", Some("Dеlеtе"), None, Destroy),
            ("x", "click", Some("P a y"), None, Spend),
            // Prices.
            ("x", "click", Some("Continue – $49.99"), None, Spend),
            ("x", "click", Some("9,99 €"), None, Spend),
            // Keys.
            ("com.apple.mail", "key", Some("cmd+enter"), None, Send),
            (
                "com.tinyspeck.slackmacgap",
                "key",
                Some("Enter"),
                None,
                Send,
            ),
            ("com.apple.terminal", "key", Some("Return"), None, Security),
            (
                "com.apple.finder",
                "key",
                Some("cmd+backspace"),
                None,
                Destroy,
            ),
            ("x", "hotkey", Some("ctrl+shift+delete"), None, Destroy),
            // Typing.
            (
                "x",
                "type",
                Some("Card number"),
                Some("4111 1111 1111 1111"),
                Spend,
            ),
            (
                "x",
                "type",
                Some("Notes"),
                Some("4111-1111-1111-1111"),
                Spend,
            ),
            ("x", "type", Some("Password"), Some("hunter2"), Security),
            (
                "com.tinyspeck.slackmacgap",
                "type",
                Some("Message"),
                Some("hi\n"),
                Send,
            ),
            (
                "com.apple.terminal",
                "type",
                None,
                Some("sudo rm -rf /"),
                Security,
            ),
            ("com.apple.terminal", "type", None, Some("ls\n"), Security),
            // Navigation.
            (
                "x",
                "open_url",
                Some("https://news.example.com/unsubscribe?id=1"),
                None,
                Destroy,
            ),
            (
                "x",
                "navigate",
                None,
                Some("https://shop.example.com/checkout"),
                Spend,
            ),
            ("x", "click", Some("tel:+15551234567"), None, Send),
            // Apps.
            (
                "com.apple.systempreferences",
                "click",
                Some("Display"),
                None,
                Security,
            ),
            ("com.apple.appstore", "click", Some("Get"), None, Security),
        ];
        for (app, verb, label, preview, want) in table {
            assert_eq!(
                risk(app, verb, *label, *preview),
                *want,
                "{app} {verb} {label:?} {preview:?}"
            );
        }
    }

    #[test]
    fn context_is_inherited_by_generic_controls() {
        use RiskClass::*;
        let payment = ActionContext::default().with_form_fields(["Card number", "CVV", "Expiry"]);
        assert_eq!(
            risk_ctx("shop.example", "click", Some("Continue"), None, &payment),
            Spend
        );
        assert_eq!(
            risk_ctx("shop.example", "click", None, None, &payment),
            Spend
        );
        assert_eq!(
            risk_ctx("shop.example", "key", Some("Enter"), None, &payment),
            Spend
        );
        // A dismissive control does not inherit.
        assert_eq!(
            risk_ctx("shop.example", "click", Some("Cancel"), None, &payment),
            Observe
        );

        let compose = ActionContext::default()
            .with_window_title("New Message")
            .with_form_fields(["To", "Subject"]);
        assert_eq!(
            risk_ctx("com.apple.mail", "click", Some("OK"), None, &compose),
            Send
        );
        assert_eq!(
            risk_ctx("com.apple.mail", "click", Some("Bold"), None, &compose),
            Observe
        );

        let dialog = ActionContext::default().with_dialog_text("Permanently delete 12 items?");
        assert_eq!(risk_ctx("x", "click", Some("Yes"), None, &dialog), Destroy);
        assert_eq!(risk_ctx("x", "click", Some("No"), None, &dialog), Observe);

        let sub = ActionContext::default()
            .with_dialog_text("Are you sure you want to cancel your subscription?");
        assert_eq!(risk_ctx("x", "click", Some("Cancel"), None, &sub), Destroy);
    }

    #[test]
    fn never_lowers_declared_risk() {
        let mut a = action("x", "scroll", None, None);
        a.declared_risk = RiskClass::Spend;
        let c = RiskClassifier::global().classify(&a);
        assert_eq!(c.risk, RiskClass::Spend);
        assert!(!c.raised());
    }

    #[test]
    fn reasons_explain_raises() {
        let c = RiskClassifier::global().classify(&action("x", "click", Some("Pay now"), None));
        assert!(c.raised());
        assert!(
            c.reasons.iter().any(|r| r.contains("spend")),
            "{:?}",
            c.reasons
        );
    }

    #[test]
    fn luhn_detection() {
        assert!(contains_card_number("4111 1111 1111 1111"));
        assert!(!contains_card_number("4111 1111 1111 1112"));
        assert!(!contains_card_number("call 555 1234"));
    }

    #[test]
    fn classification_is_fast() {
        let a = action(
            "com.apple.mail",
            "click",
            Some("Zahlungspflichtig bestellen – 49,99 €"),
            Some("hello"),
        );
        let ctx = ActionContext::default()
            .with_form_fields(["Card number", "CVV"])
            .with_window_title("Checkout")
            .with_url("https://shop.example.com/checkout");
        let c = RiskClassifier::global();
        let start = std::time::Instant::now();
        for _ in 0..200 {
            std::hint::black_box(c.classify_with_context(&a, &ctx));
        }
        // Generous bound for debug builds on slow CI: well under 5 ms per action.
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
}
