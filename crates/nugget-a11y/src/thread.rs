//! Thread extraction: find the message list around the composer in a [`Snapshot`] and turn it
//! into [`ThreadMessage`]s, plus mail recipients and subject.
//!
//! The approach is platform-neutral and runs on snapshots, so it is tested on recorded trees:
//! 1. Walk the composer's ancestors nearest-first, starting at the editor root when focus sits
//!    inside a contenteditable (a paragraph of the draft is not the composer). At each level,
//!    score every container in the sibling subtrees by how many message-like children it has,
//!    its role (`log` > `list`), its name ("Messages", "Conversation"...), and its geometry (same
//!    column as the composer and above it). Sidebars, navigation landmarks and lists of other
//!    conversations (items that are one link each) are penalised; table rows are never a
//!    thread. The first level with a good candidate wins.
//! 2. Each child item becomes a message: its text leaves are split into author, timestamp and
//!    body; summaries such as "Your iMessage, hi, 10:41" or "Alice: hi" are parsed when the
//!    item has no text children.
//! 3. `from_me` comes from the author ("You", or one of the configured self names), the item's
//!    summary, or (chat apps) bubble alignment. When one author is the user's "You", the other
//!    named authors are not the user.
//! 4. Mail (mail apps, and any page with a Subject or To field next to the composer):
//!    recipients from To/Cc/Bcc fields, the subject from the Subject field or the window title,
//!    quoted history ("On ... wrote:" and what follows, `>` lines) cut from each message, and the
//!    quoted original when the composer holds one.
//!
//! The result keeps the newest messages within [`ThreadLimits`].

use nugget_core::ipc::{AppInfo, ThreadMessage};

use crate::role::Role;
use crate::sanitize;
use crate::tree::{Node, Snapshot, editor_root_path};

/// Default cap on messages returned.
pub const MAX_MESSAGES: usize = 30;
/// Default cap on the total size of the thread (authors and texts).
pub const MAX_THREAD_BYTES: usize = 20 * 1024;
/// Cap on a single message's text.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_RECIPIENTS: usize = 50;
/// Ancestor levels searched for the message list.
const MAX_LEVELS: usize = 14;

/// App families with their own heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppFamily {
    /// Desktop mail clients: Apple Mail, Outlook, Thunderbird, Evolution, Geary...
    Mail,
    GmailWeb,
    OutlookWeb,
    Slack,
    Teams,
    IMessage,
    WhatsApp,
    Generic,
}

impl AppFamily {
    pub fn is_mail(self) -> bool {
        matches!(
            self,
            AppFamily::Mail | AppFamily::GmailWeb | AppFamily::OutlookWeb
        )
    }

    /// Chat apps that draw the user's messages as right-aligned bubbles.
    fn aligns_bubbles(self) -> bool {
        matches!(
            self,
            AppFamily::IMessage | AppFamily::WhatsApp | AppFamily::Generic
        )
    }

    /// Apps that label the user's own messages "You", so any other author is someone else.
    fn labels_self_as_you(self) -> bool {
        matches!(
            self,
            AppFamily::IMessage | AppFamily::WhatsApp | AppFamily::Teams | AppFamily::GmailWeb
        )
    }

    /// Apps whose item summaries read "Sender, text, time".
    fn comma_summaries(self) -> bool {
        matches!(self, AppFamily::IMessage | AppFamily::Teams)
    }

    /// Apps that group consecutive messages and show the author only on the first.
    fn groups_by_author(self) -> bool {
        matches!(
            self,
            AppFamily::Slack | AppFamily::Teams | AppFamily::GmailWeb
        )
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Classify the app from its id, name and (for browsers) URL.
pub fn detect_family(app: &AppInfo) -> AppFamily {
    if let Some(host) = app.url.as_deref().and_then(host_of) {
        let is = |d: &str| host == d || host.ends_with(&format!(".{d}"));
        if host == "mail.google.com" {
            return AppFamily::GmailWeb;
        }
        if is("outlook.live.com") || is("outlook.office.com") || is("outlook.office365.com") {
            return AppFamily::OutlookWeb;
        }
        if host == "app.slack.com" || is("slack.com") {
            return AppFamily::Slack;
        }
        if is("teams.microsoft.com") || is("teams.live.com") || is("teams.cloud.microsoft") {
            return AppFamily::Teams;
        }
        if host == "web.whatsapp.com" {
            return AppFamily::WhatsApp;
        }
    }
    let id = app.id.as_deref().unwrap_or("").to_ascii_lowercase();
    let name = app.name.to_ascii_lowercase();
    let any = |keys: &[&str]| keys.iter().any(|k| id.contains(k) || name.contains(k));
    if any(&["tinyspeck", "slack"]) {
        AppFamily::Slack
    } else if any(&[
        "com.microsoft.teams",
        "ms-teams",
        "msteams",
        "teams.exe",
        "microsoft teams",
    ]) || name == "teams"
    {
        AppFamily::Teams
    } else if id == "com.apple.mobilesms" || id == "com.apple.ichat" || name == "messages" {
        AppFamily::IMessage
    } else if any(&["whatsapp"]) {
        AppFamily::WhatsApp
    } else if id == "com.apple.mail"
        || any(&[
            "com.microsoft.outlook",
            "outlook.exe",
            "olk.exe",
            "thunderbird",
            "evolution",
            "geary",
            "kmail",
            "airmail",
            "com.readdle.smartemail",
            "spark",
            "mimestream",
            "canary mail",
        ])
        || name == "mail"
        || name == "microsoft outlook"
        || name == "outlook"
    {
        AppFamily::Mail
    } else {
        AppFamily::Generic
    }
}

/// Limits on the extracted thread.
#[derive(Debug, Clone)]
pub struct ThreadLimits {
    pub max_messages: usize,
    pub max_bytes: usize,
    /// The user's own names or handles, used to set `from_me` when an app shows them as author.
    pub self_names: Vec<String>,
}

impl Default for ThreadLimits {
    fn default() -> Self {
        ThreadLimits {
            max_messages: MAX_MESSAGES,
            max_bytes: MAX_THREAD_BYTES,
            self_names: Vec::new(),
        }
    }
}

/// What was found around the composer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadContext {
    pub messages: Vec<ThreadMessage>,
    pub recipients: Vec<String>,
    pub subject: Option<String>,
}

/// Extract the thread, recipients and subject around the focused element of `snap`.
/// `field_value` is the composer's (uncapped) value, used for quoted mail.
pub fn extract(
    snap: &Snapshot,
    app: &AppInfo,
    field_value: &str,
    limits: &ThreadLimits,
) -> ThreadContext {
    let family = if app.url.is_none() {
        // The engine fills the URL in; snapshots replayed on their own carry it on the web area.
        let url = snap
            .ancestors()
            .iter()
            .find_map(|(n, _)| n.attr("url").map(str::to_string));
        detect_family(&AppInfo { url, ..app.clone() })
    } else {
        detect_family(app)
    };
    let (recipients, subject) = if family.is_mail() {
        (recipients(&snap.root), subject(&snap.root, app, family))
    } else {
        compose_fields_near_composer(snap)
    };
    // A page with a To or Subject field right around the composer is a mail compose.
    let mail = family.is_mail() || !recipients.is_empty() || subject.is_some();
    let mut messages = find_container(snap)
        .map(|c| messages_from_container(c, family, mail, limits))
        .unwrap_or_default();
    if mail && messages.is_empty() {
        messages = quoted_messages(field_value);
    }
    ThreadContext {
        messages: cap_messages(messages, limits),
        recipients,
        subject,
    }
}

// ---------------------------------------------------------------------------------------------
// Container search

const MESSAGE_WORDS: &[&str] = &[
    "message",
    "conversation",
    "chat history",
    "chat content",
    "thread",
    "history",
    "transcript",
    "replies",
];
const NAVIGATION_WORDS: &[&str] = &[
    "conversations",
    "chat list",
    "list of chats",
    "chats",
    "channels",
    "channel list",
    "conversations list",
    "conversation list",
    "contacts",
    "sidebar",
    "navigation",
    "folders",
    "mailboxes",
    "favorites",
    "message list",
    "inbox",
    "search results",
    "workspace",
];

fn label_of(n: &Node) -> String {
    let mut s = String::new();
    for p in [&n.name, &n.description].into_iter().flatten() {
        s.push_str(&p.to_lowercase());
        s.push(' ');
    }
    s
}

fn contains_editable(n: &Node) -> bool {
    n.iter()
        .any(|d| d.states.editable && (d.role.is_text_input() || d.states.focused))
}

/// Rough measure of how much readable text an item holds (capped walk).
fn text_weight(n: &Node) -> (usize, usize) {
    let mut chars = 0;
    let mut leaves = 0;
    for d in n.iter().take(200) {
        if d.role.is_chrome() && d.role != Role::Image {
            continue;
        }
        if let Some(t) = d.own_text() {
            chars += t.chars().count();
            leaves += 1;
        }
    }
    if leaves == 0
        && let Some(t) = n.name.as_deref().or(n.description.as_deref())
    {
        return (t.chars().count(), 1);
    }
    (chars, leaves)
}

fn is_message_like(n: &Node) -> bool {
    if n.role.is_chrome() || n.role.is_text_input() || n.states.editable {
        return false;
    }
    let (chars, _) = text_weight(n);
    chars >= 2 && !contains_editable(n)
}

/// Labels of regions that hold other conversations or folders, whose whole subtree is
/// navigation (a narrower list than [`NAVIGATION_WORDS`], which also applies to the container
/// itself).
const NAVIGATION_REGIONS: &[&str] = &[
    "sidebar",
    "conversations",
    "chat list",
    "chats",
    "channels",
    "inbox",
    "folders",
    "mailboxes",
    "contacts",
];

/// A navigation or complementary landmark (AX subroles, AT-SPI/UIA `xml-roles`), or a region
/// labelled as a list of other conversations.
fn is_navigation_region(n: &Node) -> bool {
    let raw = n.raw_role.to_ascii_lowercase();
    if raw.contains("landmarknavigation") || raw.contains("landmarkcomplementary") {
        return true;
    }
    if n.attr("xml_roles").is_some_and(|x| {
        x.split_whitespace()
            .any(|r| r == "navigation" || r == "complementary")
    }) {
        return true;
    }
    let label = label_of(n);
    !label.is_empty() && NAVIGATION_REGIONS.iter().any(|w| label.contains(w))
}

/// A form: a secure field, a text input, a check box or combo box, or an ARIA form inside.
fn holds_form_controls(c: &Node) -> bool {
    c.iter().take(1000).any(|d| {
        d.states.secure
            || d.role.is_text_input()
            || matches!(d.role, Role::CheckBox | Role::ComboBox)
            || d.attr("xml_roles")
                .is_some_and(|x| x.split_whitespace().any(|r| r == "form"))
    })
}

/// A list box, menu, tree or radio group of options (ARIA roles on web content).
fn is_picker(c: &Node) -> bool {
    let picker_role = |n: &Node, roles: &[&str]| {
        n.attr("xml_roles")
            .is_some_and(|x| x.split_whitespace().any(|r| roles.contains(&r)))
    };
    picker_role(
        c,
        &[
            "listbox",
            "menu",
            "menubar",
            "tree",
            "radiogroup",
            "tablist",
        ],
    ) || (!c.children.is_empty()
        && c.children
            .iter()
            .all(|k| picker_role(k, &["option", "menuitem", "treeitem", "radio", "tab"])))
}

/// An item whose text all sits in one link or button (an entry in a list of conversations,
/// with its preview): following single children down to a link or button.
fn is_link_wrapped(item: &Node) -> bool {
    let mut n = item;
    for _ in 0..6 {
        if matches!(
            n.role,
            Role::Link | Role::Button | Role::MenuItem | Role::Tab
        ) {
            return true;
        }
        match n.children.as_slice() {
            [only] => n = only,
            _ => return false,
        }
    }
    false
}

fn overlap_x(a: &crate::Rect, b: &crate::Rect) -> f64 {
    let l = a.x.max(b.x);
    let r = (a.x + a.width).min(b.x + b.width);
    (r - l).max(0.0)
}

/// Score `c` as the message container. `in_nav`: it sits in a navigation region.
fn score(c: &Node, composer: Option<&Node>, precedes: bool, in_nav: bool) -> f64 {
    // A table row's cells are the columns of one record, not messages; forms (a password
    // prompt next to the composer) and pickers (a list box of recipient chips) are controls.
    if c.role == Role::Row || holds_form_controls(c) || is_picker(c) {
        return 0.0;
    }
    let items: Vec<&Node> = c.children.iter().filter(|k| is_message_like(k)).collect();
    let n = items.len();
    let label = label_of(c);
    let named_msgs = MESSAGE_WORDS.iter().any(|w| label.contains(w));
    let named_nav = NAVIGATION_WORDS.iter().any(|w| label.contains(w));
    let min_items = if c.role == Role::Log || (named_msgs && !named_nav) {
        1
    } else {
        2
    };
    if n < min_items {
        return 0.0;
    }
    let mut s = n.min(60) as f64;
    s += match c.role {
        Role::Log => 8.0,
        Role::List | Role::Table => 3.0,
        _ => 0.0,
    };
    // Penalties multiply the whole score, bonuses included.
    let mut factor = 1.0;
    if named_nav || in_nav {
        factor *= 0.2;
    } else if named_msgs {
        s += 6.0;
    }
    // Items made of a single short leaf look like navigation (channel names, folders).
    let (total_chars, structured) = items.iter().fold((0usize, 0usize), |(t, st), k| {
        let (c, leaves) = text_weight(k);
        (
            t + c,
            st + usize::from(leaves >= 2 || !k.children.is_empty()),
        )
    });
    let avg = total_chars as f64 / n as f64;
    if avg < 14.0 {
        factor *= 0.4;
    }
    if structured == 0 && !matches!(c.role, Role::Log | Role::List | Role::Table) {
        factor *= 0.5;
    }
    let wrapped = items.iter().filter(|k| is_link_wrapped(k)).count();
    if wrapped == n {
        factor *= 0.2;
    } else if wrapped * 2 >= n {
        factor *= 0.4;
    }
    if let (Some(cb), Some(fb)) = (c.bounds, composer.and_then(|f| f.bounds)) {
        if cb.width > 0.0 && fb.width > 0.0 {
            let ov = overlap_x(&cb, &fb) / fb.width.min(cb.width);
            if ov >= 0.5 {
                s += 4.0;
            } else if ov < 0.1 {
                factor *= 0.3;
            }
            if cb.y + cb.height <= fb.y + fb.height * 0.5 + 4.0 {
                s += 2.0;
            }
        }
    } else if precedes {
        s += 1.0;
    }
    s * factor
}

/// Visit the containers under `n` (pre-order, bounded), with whether each sits in a navigation
/// region.
fn visit_containers<'a>(
    n: &'a Node,
    in_nav: bool,
    budget: &mut usize,
    f: &mut impl FnMut(&'a Node, bool),
) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    if n.children.is_empty() {
        return;
    }
    let nav = in_nav || is_navigation_region(n);
    f(n, nav);
    for c in &n.children {
        visit_containers(c, nav, budget, f);
    }
}

/// Ancestors of the node at `path`, nearest first, each with the index of the child on the path.
fn ancestors_at<'a>(root: &'a Node, path: &[usize]) -> Vec<(&'a Node, usize)> {
    let mut out = Vec::with_capacity(path.len());
    let mut n = root;
    for &i in path {
        out.push((n, i));
        match n.children.get(i) {
            Some(c) => n = c,
            None => break,
        }
    }
    out.reverse();
    out
}

/// The best message container around the focused element (the editor root when focus is
/// inside one), searching outward.
pub fn find_container(snap: &Snapshot) -> Option<&Node> {
    let path = editor_root_path(snap);
    let composer = snap.node_at(&path);
    for (level, (anc, on_path)) in ancestors_at(&snap.root, &path).into_iter().enumerate() {
        if level >= MAX_LEVELS {
            break;
        }
        let mut best: Option<(f64, &Node)> = None;
        for (j, sib) in anc.children.iter().enumerate() {
            if j == on_path {
                continue;
            }
            let mut budget = 5000;
            visit_containers(sib, false, &mut budget, &mut |c, in_nav| {
                let s = score(c, composer, j < on_path, in_nav);
                if s >= 3.0 && best.is_none_or(|(b, _)| s > b) {
                    best = Some((s, c));
                }
            });
        }
        if let Some((_, c)) = best {
            return Some(c);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Items to messages

const NOISE: &[&str] = &[
    "reply",
    "reply in thread",
    "more actions",
    "more",
    "more options",
    "add reaction",
    "react",
    "forward",
    "share",
    "copy",
    "copy link",
    "edit",
    "delete",
    "like",
    "show more",
    "see more",
    "show less",
    "read more",
    "open",
    "download",
    "save",
    "pin",
    "bookmark",
    "...",
    "…",
    "edited",
    "(edited)",
    "delivered",
    "read",
    "seen",
    "sent",
];

fn is_noise(s: &str) -> bool {
    let t = s.trim().trim_end_matches(':').to_lowercase();
    t.is_empty() || NOISE.contains(&t.as_str()) || t.chars().all(|c| !c.is_alphanumeric())
}

#[derive(Debug)]
struct Leaf<'a> {
    role: Role,
    text: &'a str,
    x: Option<f64>,
    /// Identity of the parent element: leaves of one parent are runs of the same line.
    parent: usize,
}

impl Leaf<'_> {
    /// Whether `next` continues this leaf's line (text runs and links in one block).
    fn same_line(&self, next: &Leaf<'_>) -> bool {
        let inline = |r: Role| matches!(r, Role::StaticText | Role::Link);
        self.parent == next.parent && inline(self.role) && inline(next.role)
    }
}

fn collect_leaves<'a>(n: &'a Node, out: &mut Vec<Leaf<'a>>) {
    collect_leaves_in(n, 0, out);
}

fn collect_leaves_in<'a>(n: &'a Node, parent: usize, out: &mut Vec<Leaf<'a>>) {
    if out.len() >= 64 || n.states.secure {
        return;
    }
    if n.role.is_chrome() && n.role != Role::Button {
        return;
    }
    if let Some(t) = n.own_text() {
        out.push(Leaf {
            role: n.role,
            text: t,
            x: n.bounds.map(|b| b.x),
            parent,
        });
        return;
    }
    if n.role == Role::Button {
        // Author names are often buttons (Slack, Teams); keep the name as a candidate.
        if let Some(t) = n.name.as_deref().filter(|t| !is_noise(t)) {
            out.push(Leaf {
                role: Role::Button,
                text: t,
                x: n.bounds.map(|b| b.x),
                parent,
            });
        }
        return;
    }
    let id = std::ptr::from_ref(n) as usize;
    for c in &n.children {
        collect_leaves_in(c, id, out);
    }
}

const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "wed",
    "thu",
    "fri",
    "sat",
    "sun",
];
const MONTHS: &[&str] = &[
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec",
];

fn has_clock_time(s: &str) -> bool {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b':' && i >= 1 && b[i - 1].is_ascii_digit() {
            let after = &b[i + 1..];
            if after.len() >= 2 && after[0].is_ascii_digit() && after[1].is_ascii_digit() {
                let d_before = b[..i]
                    .iter()
                    .rev()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                if d_before <= 2 && (after.len() == 2 || !after[2].is_ascii_digit()) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a short string reads as a timestamp ("10:42 AM", "Yesterday", "Mon 3:15 PM",
/// "3/12/24", "2024-03-12", "5 min ago").
pub fn looks_like_timestamp(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    if t.is_empty() || t.chars().count() > 40 {
        return false;
    }
    let first = t
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .unwrap_or("");
    if has_clock_time(&t) && t.split_whitespace().count() <= 6 {
        return true;
    }
    if ["yesterday", "today", "just now", "now"].contains(&t.as_str())
        || t.starts_with("yesterday")
        || t.starts_with("today")
        || t.ends_with(" ago")
    {
        return true;
    }
    if (WEEKDAYS.contains(&first) || MONTHS.contains(&first))
        && t.split_whitespace().count() <= 4
        && t.chars().any(|c| c.is_ascii_digit())
    {
        return true;
    }
    // Numeric dates: 3/12/24, 12.03.2024, 2024-03-12
    let digits = t.chars().filter(char::is_ascii_digit).count();
    let seps = t.chars().filter(|c| matches!(c, '/' | '-' | '.')).count();
    digits >= 3 && seps >= 1 && t.chars().all(|c| c.is_ascii_digit() || "/-., ".contains(c))
}

fn looks_like_name(s: &str) -> bool {
    let t = s.trim();
    let words = t.split_whitespace().count();
    !t.is_empty()
        && t.chars().count() <= 64
        && words <= 6
        && !t.ends_with(['.', '?', '!', ','])
        && !looks_like_timestamp(t)
        && t.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '@' || c == '+')
}

fn is_self_name(author: &str, limits: &ThreadLimits) -> bool {
    let a = author.trim().trim_end_matches(':').to_lowercase();
    a == "you"
        || a == "me"
        || a.starts_with("you ")
        || a == "(you)"
        || a.ends_with(" (you)")
        || a.ends_with(" (me)")
        || limits
            .self_names
            .iter()
            .any(|n| !n.trim().is_empty() && n.trim().to_lowercase() == a)
}

#[derive(Debug, Default, Clone)]
struct Draft {
    author: Option<String>,
    text: String,
    timestamp: Option<String>,
    from_me: Option<bool>,
    /// Left edge of the message body, for bubble alignment.
    indent: Option<f64>,
}

/// Parse an item summary such as "Your iMessage, Hi there, 10:41 AM", "Message from Alice,
/// hello", "Alice: hello", "You sent: hi", and (when `comma_form`) "Alice, hello, 10:41 AM".
fn parse_summary(s: &str, comma_form: bool) -> Draft {
    let mut d = Draft::default();
    let t = s.trim();
    let lower = t.to_lowercase();
    let take_time = |parts: &mut Vec<&str>| -> Option<String> {
        if parts.len() >= 2 && parts.last().is_some_and(|p| looks_like_timestamp(p)) {
            parts.pop().map(|p| p.trim().to_string())
        } else {
            None
        }
    };
    for prefix in [
        "your imessage",
        "your message",
        "your sms",
        "sent by you",
        "you sent",
    ] {
        if lower.starts_with(prefix) {
            let rest = t[prefix.len()..].trim_start_matches([',', ':', ' ']);
            let mut parts: Vec<&str> = rest.split(", ").collect();
            d.timestamp = take_time(&mut parts);
            d.text = parts.join(", ");
            d.author = Some("You".into());
            d.from_me = Some(true);
            return d;
        }
    }
    for prefix in [
        "imessage from ",
        "message from ",
        "sms from ",
        "text message from ",
    ] {
        if lower.starts_with(prefix) {
            let rest = &t[prefix.len()..];
            let mut parts: Vec<&str> = rest.split(", ").collect();
            d.timestamp = take_time(&mut parts);
            if parts.len() >= 2 {
                d.author = Some(parts.remove(0).trim().to_string());
            }
            d.text = parts.join(", ");
            return d;
        }
    }
    if let Some((who, rest)) = t.split_once(" said: ").or_else(|| t.split_once(" said, "))
        && looks_like_name(who)
    {
        d.author = Some(who.to_string());
        let mut parts: Vec<&str> = rest.split(", ").collect();
        d.timestamp = take_time(&mut parts);
        d.text = parts.join(", ");
        return d;
    }
    if comma_form {
        let mut parts: Vec<&str> = t.split(", ").collect();
        if parts.len() >= 3 && looks_like_name(parts[0]) {
            d.timestamp = take_time(&mut parts);
            if d.timestamp.is_some() {
                d.author = Some(parts.remove(0).trim().to_string());
                d.text = parts.join(", ");
                return d;
            }
        }
    }
    if let Some((who, rest)) = t.split_once(": ")
        && looks_like_name(who)
        && !rest.trim().is_empty()
        && !has_clock_time(who)
    {
        d.author = Some(who.to_string());
        d.text = rest.to_string();
        return d;
    }
    d.text = t.to_string();
    d
}

fn draft_from_item(item: &Node, family: AppFamily) -> Option<Draft> {
    let mut leaves = Vec::new();
    collect_leaves(item, &mut leaves);
    // Drop UI noise and repeated strings.
    let mut seen: Vec<&str> = Vec::new();
    leaves.retain(|l| {
        let keep = !is_noise(l.text) && !seen.contains(&l.text);
        if keep {
            seen.push(l.text);
        }
        keep
    });
    let summary = item
        .name
        .as_deref()
        .or(item.description.as_deref())
        .filter(|s| !s.trim().is_empty());
    let has_body = leaves.iter().any(|l| l.role != Role::Button);
    if !has_body {
        let s = summary?;
        let mut d = parse_summary(s, family.comma_summaries());
        if d.text.trim().is_empty() {
            return None;
        }
        d.indent = item.bounds.map(|b| b.x);
        return Some(d);
    }
    let mut d = Draft::default();
    if let Some(i) = leaves.iter().position(|l| looks_like_timestamp(l.text)) {
        d.timestamp = Some(leaves.remove(i).text.trim().to_string());
    }
    // Buttons that are not the author are actions; the first short leaf before the body is the
    // author when there is a body after it.
    // "Notes are here:" followed by a link on the same line is the start of the body.
    let lead_in = leaves.len() >= 2
        && leaves[0].text.trim_end().ends_with(':')
        && leaves[0].same_line(&leaves[1]);
    if leaves.len() >= 2
        && !lead_in
        && looks_like_name(leaves[0].text)
        && leaves[0].role != Role::Paragraph
        && leaves[1..].iter().any(|l| l.role != Role::Button)
    {
        let mut author = leaves.remove(0).text.trim().to_string();
        // "Alice Smith" followed by "<alice@example.com>" is one author.
        if let Some(next) = leaves.first().map(|l| l.text.trim())
            && next.len() <= 120
            && next.contains('@')
            && !next.contains(char::is_whitespace)
        {
            author = format!("{author} {next}");
            leaves.remove(0);
        }
        d.author = Some(author);
    }
    let body: Vec<&Leaf<'_>> = leaves.iter().filter(|l| l.role != Role::Button).collect();
    d.indent = body.iter().filter_map(|l| l.x).reduce(f64::min);
    // Runs of one line are joined with a space, lines and blocks with `\n`.
    let mut text = String::new();
    for (i, l) in body.iter().enumerate() {
        if i > 0 {
            text.push(if body[i - 1].same_line(l) { ' ' } else { '\n' });
        }
        text.push_str(l.text.trim());
    }
    d.text = text;
    if d.text.is_empty() {
        return None;
    }
    // A summary may still say whose message it is ("Your iMessage" with text children).
    if let Some(s) = summary {
        let p = parse_summary(s, family.comma_summaries());
        if p.from_me.is_some() {
            d.from_me = p.from_me;
        }
        if d.author.is_none() && p.author.is_some() && family != AppFamily::Generic {
            d.author = p.author;
        }
    }
    Some(d)
}

/// Mark right-aligned bubbles as the user's when the items split cleanly into two columns.
fn infer_alignment(drafts: &mut [Draft], container: &Node) {
    let xs: Vec<f64> = drafts.iter().filter_map(|d| d.indent).collect();
    if xs.len() < 2 {
        return;
    }
    let min = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let max = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let width = container.bounds.map_or(max - min, |b| b.width).max(1.0);
    if max - min < width * 0.15 {
        return;
    }
    let mid = (min + max) / 2.0;
    for d in drafts.iter_mut() {
        if d.from_me.is_none()
            && let Some(x) = d.indent
        {
            d.from_me = Some(x > mid);
        }
    }
}

/// Unwrap containers whose only message-like child is itself the real list.
fn unwrap_container(mut c: &Node) -> &Node {
    for _ in 0..8 {
        let items: Vec<&Node> = c.children.iter().filter(|k| is_message_like(k)).collect();
        if items.len() == 1
            && items[0]
                .children
                .iter()
                .filter(|k| is_message_like(k))
                .count()
                >= 2
        {
            c = items[0];
        } else {
            break;
        }
    }
    c
}

/// Group consecutive messages under the last author shown (Slack-like grouping shows the author
/// on the first message only). Generic apps also draw the user's bubbles without an author, so
/// there an authorless message joins the group only when its body starts in the same column.
fn group_by_author(drafts: &mut [Draft], family: AppFamily) {
    let mut last: Option<(String, Option<f64>)> = None;
    for d in drafts.iter_mut() {
        match (&d.author, &last) {
            (Some(a), _) => last = Some((a.clone(), d.indent)),
            (None, Some((a, indent))) => {
                // Without geometry there is nothing to tell a continuation from the user's
                // own unlabelled bubble.
                let same_column = match (d.indent, *indent) {
                    (Some(x), Some(y)) => (x - y).abs() <= 8.0,
                    _ => false,
                };
                if family.groups_by_author() || same_column {
                    d.author = Some(a.clone());
                }
            }
            (None, None) => {}
        }
    }
}

/// Cut quoted history from a mail message: everything from an attribution line ("On ...
/// wrote:", "Am ... schrieb:", an Outlook "From:" + "Sent:" header) on, and `>` lines. The text
/// is kept whole when nothing else would be left.
fn strip_quoted(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let l = line.trim();
        let lower = l.to_lowercase();
        let attribution = l.len() < 300
            && (lower.ends_with("wrote:")
                || lower.ends_with("schrieb:")
                || lower.ends_with("a écrit :"));
        let outlook = lower.starts_with("from:")
            && lines.get(i + 1).is_some_and(|n| {
                let n = n.trim().to_lowercase();
                n.starts_with("sent:") || n.starts_with("date:")
            });
        if attribution || outlook {
            break;
        }
        if !l.starts_with('>') {
            kept.push(line);
        }
    }
    let out = kept.join("\n");
    if out.trim().is_empty() {
        text.to_string()
    } else {
        out
    }
}

/// Some messages are by "You" (or a self name), and every message by another named author has
/// its body in a different column from all of them.
fn self_column_is_distinct(drafts: &[Draft], limits: &ThreadLimits) -> bool {
    let is_self = |d: &Draft| d.author.as_deref().is_some_and(|a| is_self_name(a, limits));
    let mine: Vec<Option<f64>> = drafts
        .iter()
        .filter(|d| is_self(d))
        .map(|d| d.indent)
        .collect();
    if mine.is_empty() || mine.iter().any(Option::is_none) {
        return false;
    }
    drafts
        .iter()
        .filter(|d| d.author.is_some() && !is_self(d))
        .all(|d| {
            d.indent
                .is_some_and(|x| mine.iter().flatten().all(|m| (x - m).abs() > 8.0))
        })
}

/// Drop a leading "to me, Priya" line (who a mail was sent to) from a message body.
fn strip_recipient_line(text: &str) -> String {
    match text.split_once('\n') {
        Some((first, rest)) if !rest.trim().is_empty() => {
            let f = first.trim().to_lowercase();
            if f.starts_with("to me") || f.starts_with("to you") {
                rest.to_string()
            } else {
                text.to_string()
            }
        }
        _ => text.to_string(),
    }
}

fn messages_from_container(
    container: &Node,
    family: AppFamily,
    mail: bool,
    limits: &ThreadLimits,
) -> Vec<ThreadMessage> {
    let container = unwrap_container(container);
    let mut drafts: Vec<Draft> = container
        .children
        .iter()
        .filter(|k| is_message_like(k))
        .filter_map(|k| draft_from_item(k, family))
        .collect();
    // Day separators ("Yesterday", "Monday 3 March") are not messages.
    drafts.retain(|d| !(d.author.is_none() && looks_like_timestamp(&d.text)));
    if mail {
        for d in &mut drafts {
            d.text = strip_recipient_line(&strip_quoted(&d.text));
        }
    }
    if family.groups_by_author() || family == AppFamily::Generic {
        group_by_author(&mut drafts, family);
    }
    // An app that shows the user as "You" shows everyone else by name. For apps not known to
    // do so, only when the user's messages sit in a column of their own.
    let labels_you = family.labels_self_as_you() || self_column_is_distinct(&drafts, limits);
    for d in &mut drafts {
        if d.from_me.is_none()
            && let Some(a) = &d.author
        {
            if is_self_name(a, limits) {
                d.from_me = Some(true);
            } else if labels_you || !limits.self_names.is_empty() {
                d.from_me = Some(false);
            }
        }
    }
    if family.aligns_bubbles() {
        infer_alignment(&mut drafts, container);
    }
    drafts
        .into_iter()
        .map(|d| ThreadMessage {
            author: sanitize::clean_opt(d.author.as_deref(), sanitize::MAX_SHORT),
            text: sanitize::clean_text(&d.text, MAX_MESSAGE_BYTES),
            from_me: d.from_me,
            timestamp: sanitize::clean_opt(d.timestamp.as_deref(), 64),
        })
        .filter(|m| !m.text.is_empty())
        .collect()
}

/// Keep the newest messages within the count and byte caps, oldest first.
pub fn cap_messages(mut messages: Vec<ThreadMessage>, limits: &ThreadLimits) -> Vec<ThreadMessage> {
    if messages.len() > limits.max_messages {
        messages.drain(..messages.len() - limits.max_messages);
    }
    let mut budget = limits.max_bytes;
    let mut keep_from = messages.len();
    for (i, m) in messages.iter_mut().enumerate().rev() {
        let overhead =
            m.author.as_ref().map_or(0, String::len) + m.timestamp.as_ref().map_or(0, String::len);
        let size = m.text.len() + overhead;
        if size <= budget {
            budget -= size;
            keep_from = i;
            continue;
        }
        if budget > overhead + 64 {
            // Keep the end of the oldest message that still fits partially.
            let room = budget - overhead;
            let mut start = m.text.len() - room;
            while !m.text.is_char_boundary(start) {
                start += 1;
            }
            m.text = format!("…{}", &m.text[start..]);
            keep_from = i;
        }
        break;
    }
    messages.drain(..keep_from);
    messages
}

// ---------------------------------------------------------------------------------------------
// Mail

fn field_label(n: &Node) -> Option<String> {
    let l = n
        .name
        .as_deref()
        .or(n.description.as_deref())
        .or(n.placeholder.as_deref())?;
    Some(l.trim().trim_end_matches(':').trim().to_lowercase())
}

/// Recipient labels as a whole ("To", "Cc", "To recipients"...).
fn is_recipient_label_exact(l: &str) -> bool {
    matches!(
        l,
        "to" | "cc"
            | "bcc"
            | "an"
            | "à"
            | "para"
            | "kopie"
            | "cci"
            | "recipients"
            | "to recipients"
            | "cc recipients"
            | "bcc recipients"
    )
}

fn is_recipient_label(l: &str) -> bool {
    if l.starts_with("to do") || l.starts_with("to-do") || l.starts_with("todo") {
        return false;
    }
    matches!(
        l,
        "to" | "cc" | "bcc" | "an" | "à" | "para" | "kopie" | "cci"
    ) || l.starts_with("to ")
        || l.starts_with("cc ")
        || l.starts_with("bcc ")
        || l == "recipients"
        || l.ends_with(" recipients")
}

fn split_recipients(s: &str, out: &mut Vec<String>) {
    for part in s.split([',', ';', '\n', '\u{FFFC}']) {
        let p = sanitize::clean_line(part, sanitize::MAX_SHORT);
        let lower = p.to_lowercase();
        if p.is_empty()
            || is_recipient_label(lower.trim_end_matches(':'))
            || lower.starts_with("remove")
            || lower == "add"
            || is_noise(&p)
            || out.contains(&p)
        {
            continue;
        }
        if out.len() < MAX_RECIPIENTS {
            out.push(p);
        }
    }
}

fn collect_recipient_tokens(field: &Node, out: &mut Vec<String>) {
    if let Some(v) = &field.value {
        split_recipients(v, out);
    }
    for d in field.iter().skip(1).take(200) {
        if d.states.editable && d.role.is_text_input() {
            if let Some(v) = &d.value {
                split_recipients(v, out);
            }
            continue;
        }
        let t = d.own_text().or(match d.role {
            Role::Button | Role::ListItem | Role::Link | Role::Unknown | Role::Group => {
                d.name.as_deref()
            }
            _ => None,
        });
        if let Some(t) = t {
            split_recipients(t, out);
        }
    }
}

/// Recipients from To/Cc/Bcc fields: labelled inputs or token groups, or an input that follows
/// a "To:" label.
pub fn recipients(root: &Node) -> Vec<String> {
    recipients_in(root, false, false)
}

/// Ancestor levels above the composer searched for To/Subject fields in apps that are not
/// known mail clients (never the whole page: the search stops below a web area or document).
const COMPOSE_FIELD_LEVELS: usize = 3;

/// Recipients and subject for an app that is not a known mail client: only from fields right
/// around the composer (its nearest ancestors), labelled exactly ("To", "Cc", "Subject"...) and
/// holding an editable input, so a "To Do" list or a search form elsewhere on the page does not
/// count.
fn compose_fields_near_composer(snap: &Snapshot) -> (Vec<String>, Option<String>) {
    let path = editor_root_path(snap);
    for up in 1..=COMPOSE_FIELD_LEVELS.min(path.len()) {
        let Some(scope) = snap.node_at(&path[..path.len() - up]) else {
            break;
        };
        if matches!(
            scope.role,
            Role::WebArea | Role::Document | Role::Window | Role::Application
        ) {
            break;
        }
        let recipients = recipients_in(scope, true, true);
        let subject = subject_field(scope, true);
        if !recipients.is_empty() || subject.is_some() {
            // A compose: chips in a list labelled "To" count too, labels still exact.
            return (recipients_in(scope, true, false), subject);
        }
    }
    (Vec::new(), None)
}

/// An editable text input, or a token field holding one.
fn holds_editable_input(n: &Node) -> bool {
    n.iter()
        .take(200)
        .any(|d| d.states.editable && d.role.is_text_input() && !d.states.secure)
}

/// `exact`: labels must be recipient labels as a whole; `need_input`: the field must hold an
/// editable input.
fn recipients_in(root: &Node, exact: bool, need_input: bool) -> Vec<String> {
    let mut out = Vec::new();
    let label_ok = |l: &str| {
        if exact {
            is_recipient_label_exact(l)
        } else {
            is_recipient_label(l)
        }
    };
    let field_ok = |c: &Node| !need_input || holds_editable_input(c);
    fn visit(
        n: &Node,
        out: &mut Vec<String>,
        budget: &mut usize,
        label_ok: &dyn Fn(&str) -> bool,
        field_ok: &dyn Fn(&Node) -> bool,
    ) {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let mut label_pending = false;
        for c in &n.children {
            let lbl = field_label(c);
            let is_input_like = c.role.is_text_input()
                || matches!(
                    c.role,
                    Role::Group | Role::List | Role::Section | Role::Unknown
                );
            if is_input_like && lbl.as_deref().is_some_and(label_ok) && field_ok(c) {
                collect_recipient_tokens(c, out);
                label_pending = false;
                continue;
            }
            if label_pending && is_input_like && field_ok(c) {
                collect_recipient_tokens(c, out);
                label_pending = false;
                continue;
            }
            label_pending = c.role == Role::StaticText
                && c.own_text().is_some_and(|t| {
                    label_ok(
                        t.trim()
                            .trim_end_matches(':')
                            .trim()
                            .to_lowercase()
                            .as_str(),
                    )
                });
            visit(c, out, budget, label_ok, field_ok);
        }
    }
    let mut budget = 5000;
    visit(root, &mut out, &mut budget, &label_ok, &field_ok);
    out
}

fn strip_mail_title(title: &str) -> Option<String> {
    let t = title.trim();
    // Gmail: "Re: Lunch - alice@example.com - Gmail"; Outlook web: "Mail - Name - Outlook".
    let t = t
        .strip_suffix(" - Gmail")
        .and_then(|r| r.rsplit_once(" - ").map(|(s, _)| s))
        .unwrap_or(t);
    let lower = t.to_lowercase();
    let prefixed = ["re:", "fwd:", "fw:", "aw:", "wg:", "tr:", "rv:", "sv:"]
        .iter()
        .any(|p| lower.starts_with(p));
    prefixed.then(|| sanitize::clean_line(t, sanitize::MAX_SHORT * 2))
}

/// The value of a field labelled "Subject". `strict`: the label exactly, on an editable text
/// input.
fn subject_field(root: &Node, strict: bool) -> Option<String> {
    for n in root.iter().take(5000) {
        if strict {
            if !(n.role.is_text_input() && n.states.editable) || n.states.secure {
                continue;
            }
        } else if !(n.role.is_text_input() || n.role == Role::Unknown || n.role == Role::Group) {
            continue;
        }
        let lbl = field_label(n);
        if lbl.as_deref().is_some_and(|l| {
            l == "subject"
                || l == "betreff"
                || l == "objet"
                || l == "asunto"
                || (!strict && l.starts_with("subject "))
        }) && let Some(v) = n.value.as_deref().filter(|v| !v.trim().is_empty())
        {
            return Some(sanitize::clean_line(v, sanitize::MAX_SHORT * 2));
        }
    }
    None
}

/// The subject: a field labelled "Subject", else a reply-style window title.
pub fn subject(root: &Node, app: &AppInfo, family: AppFamily) -> Option<String> {
    if let Some(s) = subject_field(root, false) {
        return Some(s);
    }
    let title = app.window_title.as_deref()?;
    match family {
        AppFamily::Mail | AppFamily::GmailWeb | AppFamily::OutlookWeb => strip_mail_title(title),
        _ => None,
    }
}

/// Split quoted mail ("On Tue, Alice <a@x> wrote:" + "> ..." lines, or an Outlook "From:/Sent:"
/// header block) out of a composer value.
pub fn quoted_messages(value: &str) -> Vec<ThreadMessage> {
    let lines: Vec<&str> = value.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let l = line.trim();
        let lower = l.to_lowercase();
        // "On <date>, <name> wrote:" / "<name> wrote:" / "Am ... schrieb ...:"
        if (lower.ends_with("wrote:")
            || lower.ends_with("schrieb:")
            || lower.ends_with("a écrit :"))
            && l.len() < 300
        {
            let head = l
                .trim_end_matches(':')
                .trim_end_matches(" wrote")
                .trim_end_matches(" schrieb")
                .trim_end_matches(" a écrit ")
                .trim();
            // "On Tue, 3 Mar 2026 at 10:00, Alice Smith <alice@x.com>" -> author after last ", "
            let (ts, who) = match head
                .strip_prefix("On ")
                .or_else(|| head.strip_prefix("Am "))
            {
                Some(rest) => match rest.rsplit_once(", ") {
                    Some((date, who)) => (Some(date.trim()), who.trim()),
                    None => (None, rest.trim()),
                },
                None => (None, head),
            };
            let body: Vec<&str> = lines[i + 1..]
                .iter()
                .map(|l| {
                    let t = l.trim_start();
                    t.strip_prefix('>')
                        .map_or(t, |r| r.strip_prefix(' ').unwrap_or(r))
                })
                .collect();
            let text = sanitize::clean_text(&body.join("\n"), MAX_MESSAGE_BYTES);
            if text.is_empty() {
                return Vec::new();
            }
            return vec![ThreadMessage {
                author: sanitize::clean_opt(Some(who), sanitize::MAX_SHORT),
                text,
                from_me: None,
                timestamp: sanitize::clean_opt(ts, 64),
            }];
        }
        if lower.starts_with("from:") && i + 1 < lines.len() {
            let next = lines[i + 1].trim().to_lowercase();
            if next.starts_with("sent:") || next.starts_with("date:") {
                let who = l[5..].trim();
                let ts = lines[i + 1].trim()[5..].trim();
                let mut j = i + 2;
                while j < lines.len() {
                    let h = lines[j].trim().to_lowercase();
                    if h.is_empty() {
                        break;
                    }
                    let is_header = ["to:", "cc:", "subject:", "bcc:"]
                        .iter()
                        .any(|p| h.starts_with(p));
                    if !is_header {
                        break;
                    }
                    j += 1;
                }
                let text = sanitize::clean_text(
                    &lines[j.min(lines.len())..].join("\n"),
                    MAX_MESSAGE_BYTES,
                );
                if text.is_empty() {
                    return Vec::new();
                }
                return vec![ThreadMessage {
                    author: sanitize::clean_opt(Some(who), sanitize::MAX_SHORT),
                    text,
                    from_me: None,
                    timestamp: sanitize::clean_opt(Some(ts), 64),
                }];
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, url: Option<&str>) -> AppInfo {
        AppInfo {
            id: Some(id.into()),
            name: name.into(),
            window_title: None,
            url: url.map(Into::into),
        }
    }

    #[test]
    fn families() {
        assert_eq!(
            detect_family(&app("com.apple.mail", "Mail", None)),
            AppFamily::Mail
        );
        assert_eq!(
            detect_family(&app(
                "com.google.Chrome",
                "Google Chrome",
                Some("https://mail.google.com/mail/u/0/#inbox")
            )),
            AppFamily::GmailWeb
        );
        assert_eq!(
            detect_family(&app(
                "msedge.exe",
                "Edge",
                Some("https://outlook.office.com/mail/")
            )),
            AppFamily::OutlookWeb
        );
        assert_eq!(
            detect_family(&app("com.tinyspeck.slackmacgap", "Slack", None)),
            AppFamily::Slack
        );
        assert_eq!(
            detect_family(&app("ms-teams.exe", "Microsoft Teams", None)),
            AppFamily::Teams
        );
        assert_eq!(
            detect_family(&app("com.apple.MobileSMS", "Messages", None)),
            AppFamily::IMessage
        );
        assert_eq!(
            detect_family(&app("WhatsApp.exe", "WhatsApp", None)),
            AppFamily::WhatsApp
        );
        assert_eq!(
            detect_family(&app(
                "firefox",
                "Firefox",
                Some("https://evil.example/mail.google.com")
            )),
            AppFamily::Generic
        );
        assert_eq!(
            detect_family(&app("thunderbird", "Thunderbird", None)),
            AppFamily::Mail
        );
    }

    #[test]
    fn timestamps() {
        for s in [
            "10:42 AM",
            "10:42",
            "Yesterday",
            "Yesterday at 9:01 PM",
            "Mon 3:15 PM",
            "3/12/24",
            "2024-03-12",
            "5 min ago",
            "Mar 3, 2026",
            "14:05:33",
        ] {
            assert!(looks_like_timestamp(s), "{s}");
        }
        for s in [
            "Alice",
            "Are we still on for lunch?",
            "Meeting at 10:30 tomorrow, bring slides please ok",
            "1234",
            "v1.2",
        ] {
            assert!(!looks_like_timestamp(s), "{s}");
        }
    }

    #[test]
    fn summaries() {
        let d = parse_summary("Your iMessage, Sounds good, see you then, 10:41 AM", true);
        assert_eq!(d.from_me, Some(true));
        assert_eq!(d.text, "Sounds good, see you then");
        assert_eq!(d.timestamp.as_deref(), Some("10:41 AM"));
        let d = parse_summary("Message from Alice Smith, Lunch tomorrow?, 9:00", false);
        assert_eq!(d.author.as_deref(), Some("Alice Smith"));
        assert_eq!(d.text, "Lunch tomorrow?");
        assert_eq!(d.from_me, None);
        let d = parse_summary("Bob: on my way", false);
        assert_eq!(d.author.as_deref(), Some("Bob"));
        assert_eq!(d.text, "on my way");
        let d = parse_summary("Alice Smith, The new place on 5th, 10:41 AM", true);
        assert_eq!(d.author.as_deref(), Some("Alice Smith"));
        assert_eq!(d.text, "The new place on 5th");
        // Without the comma form, a comma list stays text.
        let d = parse_summary("Hi, how are you, 10:00", false);
        assert_eq!(d.author, None);
        let d = parse_summary("Just some text", false);
        assert_eq!(d.author, None);
    }

    #[test]
    fn quoted_reply() {
        let v = "Sounds good!\n\nOn Tue, 3 Mar 2026 at 10:00, Alice Smith <alice@example.com> wrote:\n> Are we still on?\n> Bring slides.\n";
        let m = quoted_messages(v);
        assert_eq!(m.len(), 1);
        assert_eq!(
            m[0].author.as_deref(),
            Some("Alice Smith <alice@example.com>")
        );
        assert_eq!(m[0].timestamp.as_deref(), Some("Tue, 3 Mar 2026 at 10:00"));
        assert_eq!(m[0].text, "Are we still on?\nBring slides.");

        let v = "Thanks\n\nFrom: Bob <bob@x.com>\nSent: Monday, March 2, 2026 9:00 AM\nTo: Me\nSubject: Q1\n\nNumbers attached.";
        let m = quoted_messages(v);
        assert_eq!(m[0].author.as_deref(), Some("Bob <bob@x.com>"));
        assert_eq!(m[0].text, "Numbers attached.");
        assert!(quoted_messages("no quote here").is_empty());
    }

    #[test]
    fn caps_keep_newest() {
        let msgs: Vec<ThreadMessage> = (0..50)
            .map(|i| ThreadMessage {
                text: format!("message number {i}"),
                ..Default::default()
            })
            .collect();
        let out = cap_messages(msgs, &ThreadLimits::default());
        assert_eq!(out.len(), MAX_MESSAGES);
        assert_eq!(out.last().unwrap().text, "message number 49");
        assert_eq!(out[0].text, "message number 20");

        let big: Vec<ThreadMessage> = (0..10)
            .map(|i| ThreadMessage {
                text: format!("{i}{}", "x".repeat(3000)),
                ..Default::default()
            })
            .collect();
        let out = cap_messages(big, &ThreadLimits::default());
        let total: usize = out.iter().map(|m| m.text.len()).sum();
        assert!(total <= MAX_THREAD_BYTES + 4, "{total}");
        assert!(out.last().unwrap().text.starts_with('9'));
        assert!(out[0].text.starts_with('…'));
    }

    #[test]
    fn mail_title_subjects() {
        assert_eq!(
            strip_mail_title("Re: Lunch - alice@example.com - Gmail").as_deref(),
            Some("Re: Lunch")
        );
        assert_eq!(
            strip_mail_title("Fwd: Q1 numbers").as_deref(),
            Some("Fwd: Q1 numbers")
        );
        assert_eq!(strip_mail_title("Inbox - alice@example.com - Gmail"), None);
    }

    // -----------------------------------------------------------------------------------------
    // Recorded-tree fixtures (tests/fixtures/trees)

    #[derive(serde::Deserialize)]
    struct Fixture {
        app: AppInfo,
        #[serde(default)]
        field_value: String,
    }

    fn run(name: &str, limits: &ThreadLimits) -> (ThreadContext, AppInfo) {
        let path = format!(
            "{}/tests/fixtures/trees/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let json = std::fs::read_to_string(&path).unwrap();
        let fx: Fixture = serde_json::from_str(&json).unwrap();
        let snap = Snapshot::from_json(&json).unwrap();
        assert!(
            snap.focused().is_some_and(|f| f.states.focused),
            "{name}: focus path"
        );
        (extract(&snap, &fx.app, &fx.field_value, limits), fx.app)
    }

    fn summary(ctx: &ThreadContext) -> Vec<(Option<&str>, &str, Option<bool>)> {
        ctx.messages
            .iter()
            .map(|m| (m.author.as_deref(), m.text.as_str(), m.from_me))
            .collect()
    }

    #[test]
    fn fixture_generic_chat() {
        let (ctx, _) = run("generic_chat", &ThreadLimits::default());
        assert_eq!(
            summary(&ctx),
            vec![
                (
                    Some("Alice"),
                    "Are we still on for lunch tomorrow?",
                    Some(false)
                ),
                (Some("Bob"), "I can bring the slides.", Some(false)),
                (Some("You"), "Yes, noon works for me.", Some(true)),
            ]
        );
        assert!(ctx.recipients.is_empty());
        assert_eq!(ctx.subject, None);
    }

    #[test]
    fn fixture_slack_groups_authors_and_skips_sidebar() {
        let limits = ThreadLimits {
            self_names: vec!["Tanuj".into()],
            ..Default::default()
        };
        let (ctx, app) = run("slack_mac", &limits);
        assert_eq!(detect_family(&app), AppFamily::Slack);
        assert_eq!(
            summary(&ctx),
            vec![
                (
                    Some("Alice Smith"),
                    "Can someone review the a11y PR today?",
                    Some(false)
                ),
                (
                    Some("Alice Smith"),
                    "It touches the AT-SPI backend.",
                    Some(false)
                ),
                (Some("Tanuj"), "On it, will look after standup.", Some(true)),
                (
                    Some("Bob Jones"),
                    "Thanks! Ping me if the tests are flaky.",
                    Some(false)
                ),
            ]
        );
        assert_eq!(ctx.messages[0].timestamp.as_deref(), Some("4:58 PM"));
        // Without self names, from_me stays unknown for named authors.
        let (ctx, _) = run("slack_mac", &ThreadLimits::default());
        assert!(ctx.messages.iter().all(|m| m.from_me.is_none()));
    }

    #[test]
    fn fixture_imessage_summaries() {
        let (ctx, _) = run("imessage", &ThreadLimits::default());
        assert_eq!(
            summary(&ctx),
            vec![
                (
                    Some("Alice Smith"),
                    "Are we still on for lunch tomorrow?",
                    Some(false)
                ),
                (Some("Alice Smith"), "The new place on 5th", Some(false)),
                (Some("You"), "Yes! Noon works", Some(true)),
            ]
        );
        assert_eq!(ctx.messages[2].timestamp.as_deref(), Some("10:43 AM"));
    }

    #[test]
    fn fixture_whatsapp_alignment() {
        let (ctx, _) = run("whatsapp_windows", &ThreadLimits::default());
        assert_eq!(
            summary(&ctx),
            vec![
                (None, "Did you get the tickets?", Some(false)),
                (None, "Yes, two for Friday", Some(true)),
                (None, "Amazing, thank you!", Some(false)),
            ]
        );
        assert_eq!(ctx.messages[1].timestamp.as_deref(), Some("09:15"));
    }

    #[test]
    fn fixture_gmail_thread_recipients_subject() {
        // The fixture's app has no URL: the family comes from the web area's URL.
        let (ctx, _) = run("gmail_compose", &ThreadLimits::default());
        assert_eq!(
            summary(&ctx),
            vec![
                (
                    Some("Alice Smith <alice@example.com>"),
                    "Shall we do lunch on Friday? The usual place.",
                    Some(false)
                ),
                (Some("me"), "Sounds good. 12:30?", Some(true)),
                (
                    Some("Alice Smith <alice@example.com>"),
                    "Perfect, see you there. Bringing Bob.",
                    Some(false)
                ),
            ]
        );
        assert_eq!(
            ctx.recipients,
            vec![
                "Alice Smith <alice@example.com>",
                "Bob Jones <bob@example.com>"
            ]
        );
        assert_eq!(ctx.subject.as_deref(), Some("Re: Lunch on Friday"));
    }

    #[test]
    fn fixture_apple_mail_quoted_reply() {
        let (ctx, _) = run("apple_mail_reply", &ThreadLimits::default());
        assert_eq!(ctx.recipients, vec!["Bob Jones", "carol@example.com"]);
        assert_eq!(ctx.subject.as_deref(), Some("Re: Q1 numbers"));
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(
            ctx.messages[0].author.as_deref(),
            Some("Bob Jones <bob@example.com>")
        );
        assert_eq!(
            ctx.messages[0].text,
            "Q1 numbers attached.\nLet me know if anything looks off."
        );
    }

    #[test]
    fn fixture_recorded_gtk_tree() {
        // A real tree recorded over AT-SPI (no hand-written bounds or roles).
        let (ctx, _) = run("gtk_recorded_atspi", &ThreadLimits::default());
        assert_eq!(
            summary(&ctx),
            vec![
                (
                    Some("Alice"),
                    "Are we still on for lunch tomorrow?",
                    Some(false)
                ),
                (Some("Bob"), "I can bring the slides.", Some(false)),
                (Some("You"), "Yes, noon works for me.", Some(true)),
            ]
        );
    }

    #[test]
    fn fixture_limits_apply() {
        let limits = ThreadLimits {
            max_messages: 2,
            ..Default::default()
        };
        let (ctx, _) = run("generic_chat", &limits);
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[1].text, "Yes, noon works for me.");
    }

    // -----------------------------------------------------------------------------------------
    // Web editor fixtures (the FF3 app-matrix pages, modelled per platform)

    /// The chat context of the FF3 pages, oldest first. Everyone's messages sit in one column
    /// (Slack-like), so for a generic page only "You" is known; the others stay unknown.
    fn chat_truth() -> Vec<(Option<&'static str>, &'static str, Option<bool>)> {
        vec![
            (
                Some("Maya Chen"),
                "Morning! Did everyone get a chance to look at the venue options?",
                None,
            ),
            (
                Some("Maya Chen"),
                "The second one has a terrace and fits all twelve of us.",
                None,
            ),
            (
                Some("Sam Ortiz"),
                "Terrace sounds great for September.\nCan we check the catering menu first?",
                None,
            ),
            (
                Some("You"),
                "Let's book it, I'll ask about catering.",
                Some(true),
            ),
        ]
    }

    #[test]
    fn fixture_web_lexical_chat_ax() {
        let (ctx, _) = run("web_lexical_chat_ax", &ThreadLimits::default());
        assert_eq!(summary(&ctx), chat_truth());
        assert_eq!(ctx.messages[0].timestamp.as_deref(), Some("10:02 AM"));
        assert_eq!(ctx.messages[1].timestamp.as_deref(), Some("10:03 AM"));
        assert!(ctx.recipients.is_empty());
        assert_eq!(ctx.subject, None);
    }

    #[test]
    fn fixture_web_prosemirror_atspi_focus_in_paragraph() {
        let (ctx, _) = run("web_prosemirror_atspi", &ThreadLimits::default());
        assert_eq!(summary(&ctx), chat_truth());
        assert_eq!(ctx.messages[3].timestamp.as_deref(), Some("10:20 AM"));
        assert!(ctx.recipients.is_empty());
    }

    #[test]
    fn fixture_web_ckeditor_mail_uia() {
        let (ctx, _) = run("web_ckeditor_mail_uia", &ThreadLimits::default());
        assert_eq!(
            summary(&ctx),
            vec![
                (
                    Some("Maya Chen <maya@example.com>"),
                    "Hi Jordan,\nCould we hold the offsite at the lakeside venue this year? It has room for everyone.",
                    None
                ),
                (
                    Some("Jordan Lee <jordan@example.com>"),
                    "Lakeside works for me. Thursday is better than Friday though.",
                    None
                ),
            ]
        );
        assert_eq!(
            ctx.messages[1].timestamp.as_deref(),
            Some("Tue, Oct 28, 9:15 AM")
        );
        assert_eq!(ctx.recipients, vec!["Maya Chen"]);
        assert_eq!(ctx.subject.as_deref(), Some("Re: Q3 offsite venue"));
    }

    #[test]
    fn fixture_web_mail_compose_ax() {
        let (ctx, _) = run("web_mail_compose_ax", &ThreadLimits::default());
        // The inbox of other conversations is not this thread.
        assert_eq!(summary(&ctx), vec![]);
        assert_eq!(ctx.recipients, vec!["Jordan Lee"]);
        assert_eq!(ctx.subject.as_deref(), Some("Q3 offsite venue"));
    }

    fn load(name: &str) -> (Snapshot, AppInfo) {
        let path = format!(
            "{}/tests/fixtures/trees/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let json = std::fs::read_to_string(&path).unwrap();
        let fx: Fixture = serde_json::from_str(&json).unwrap();
        (Snapshot::from_json(&json).unwrap(), fx.app)
    }

    fn static_text(s: &str, bounds: [f64; 4]) -> Node {
        Node {
            role: Role::StaticText,
            value: Some(s.into()),
            bounds: Some(crate::Rect {
                x: bounds[0],
                y: bounds[1],
                width: bounds[2],
                height: bounds[3],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn decoy_sidebar_alone_is_not_a_thread() {
        for (name, log_at) in [
            ("web_lexical_chat_ax", vec![1, 0, 1]),
            ("web_prosemirror_atspi", vec![1, 0, 1]),
        ] {
            let (mut snap, app) = load(name);
            // Remove the chat log: what is left (the sidebar of other conversations with
            // previews, the status line, the toolbar) is not a thread.
            let mut main = &mut snap.root;
            for &i in &log_at {
                main = &mut main.children[i];
            }
            main.children.remove(1);
            snap.focus_path[log_at.len()] -= 1;
            assert!(snap.focused().is_some_and(|f| f.states.focused), "{name}");
            let ctx = extract(&snap, &app, "", &ThreadLimits::default());
            assert_eq!(summary(&ctx), vec![], "{name}");
        }
    }

    #[test]
    fn formatted_draft_paragraphs_are_not_a_thread() {
        // Focus on a text run inside a Lexical paragraph (Chromium after a click), the draft
        // holding another paragraph of several formatted runs, and no chat log on the page.
        let (mut snap, app) = load("web_lexical_chat_ax");
        let main = &mut snap.root.children[1].children[0].children[1];
        main.children.remove(1);
        let editor = &mut main.children[3];
        editor.states.focused = false;
        let mut runs = Node::new(Role::Group);
        runs.states.editable = true;
        runs.bounds = Some(crate::Rect {
            x: 340.0,
            y: 700.0,
            width: 800.0,
            height: 18.0,
        });
        runs.children = vec![
            static_text(
                "The first part of the draft says ",
                [340.0, 700.0, 220.0, 18.0],
            ),
            static_text(
                "something in bold that matters",
                [560.0, 700.0, 200.0, 18.0],
            ),
            static_text(
                " and then it carries on for a while",
                [760.0, 700.0, 240.0, 18.0],
            ),
        ];
        let mut leaf = static_text("Second paragraph", [340.0, 720.0, 200.0, 18.0]);
        leaf.states.focused = true;
        leaf.states.editable = true;
        let mut second = Node::new(Role::Group);
        second.states.editable = true;
        second.children = vec![leaf];
        editor.children = vec![runs, second];
        snap.focus_path = vec![1, 0, 1, 3, 1, 0];
        assert!(snap.focused().is_some_and(|f| f.states.focused));
        assert_eq!(crate::tree::editor_root_path(&snap), vec![1, 0, 1, 3]);
        let ctx = extract(&snap, &app, "", &ThreadLimits::default());
        assert_eq!(summary(&ctx), vec![]);
    }

    #[test]
    fn quoted_history_is_cut_from_mail_messages() {
        assert_eq!(
            strip_quoted(
                "Works for me.\nOn Mon, Oct 27, 2025 at 4:02 PM Maya <m@x.com> wrote:\nHi"
            ),
            "Works for me."
        );
        assert_eq!(
            strip_quoted("Sure\n> earlier\n> text\nThanks"),
            "Sure\nThanks"
        );
        assert_eq!(
            strip_quoted("Numbers below.\nFrom: Bob <bob@x.com>\nSent: Monday\nQ1"),
            "Numbers below."
        );
        // Nothing but a quote: kept as it is.
        assert_eq!(strip_quoted("> only quoted"), "> only quoted");
    }

    #[test]
    fn to_do_lists_and_far_subject_fields_are_not_a_compose() {
        let (mut snap, app) = load("web_lexical_chat_ax");
        // A "To Do" widget right next to the composer, and a "Subject" input in the sidebar.
        let mut todo = Node::new(Role::Group);
        todo.name = Some("To Do".into());
        for t in ["Book the venue", "Order catering"] {
            let mut b = Node::new(Role::Button);
            b.name = Some(t.into());
            todo.children.push(b);
        }
        let mut add = Node::new(Role::TextField);
        add.name = Some("Add a task".into());
        add.states.editable = true;
        todo.children.push(add);
        let mut subject = Node::new(Role::TextField);
        subject.name = Some("Subject".into());
        subject.value = Some("Weekly notes".into());
        subject.states.editable = true;
        let web = &mut snap.root.children[1].children[0];
        web.children[0].children.push(subject);
        // A read-only "To" header with a name after it, also near the composer.
        let mut to = Node::new(Role::StaticText);
        to.value = Some("To".into());
        let mut who = Node::new(Role::Group);
        who.children = vec![static_text("Design team", [0.0, 0.0, 10.0, 10.0])];
        let main = &mut web.children[1];
        main.children.insert(3, todo);
        main.children.insert(3, who);
        main.children.insert(3, to);
        snap.focus_path[3] += 3;
        assert!(snap.focused().is_some_and(|f| f.states.focused));
        let ctx = extract(&snap, &app, "", &ThreadLimits::default());
        assert!(ctx.recipients.is_empty(), "{:?}", ctx.recipients);
        assert_eq!(ctx.subject, None);
        assert_eq!(ctx.messages.len(), 4);
        // Mail clients do not take a "To Do" list for recipients either.
        assert!(!is_recipient_label("to do"));
        assert!(!is_recipient_label("to-do list"));
        assert!(is_recipient_label("to recipients"));
    }

    #[test]
    fn you_marks_others_only_when_the_user_has_a_column() {
        let draft = |author: &str, x: Option<f64>| Draft {
            author: Some(author.into()),
            text: "text".into(),
            indent: x,
            ..Default::default()
        };
        let limits = ThreadLimits::default();
        // One column for everyone (Slack-like): "You" says nothing about the others.
        let same = [draft("Maya", Some(60.0)), draft("You", Some(60.0))];
        assert!(!self_column_is_distinct(&same, &limits));
        // No geometry: unknown.
        let none = [draft("Maya", None), draft("You", None)];
        assert!(!self_column_is_distinct(&none, &limits));
        // Bubbles: the user's on the right.
        let bubbles = [draft("Maya", Some(60.0)), draft("You", Some(420.0))];
        assert!(self_column_is_distinct(&bubbles, &limits));
    }

    // -----------------------------------------------------------------------------------------
    // Trees recorded from Chrome on Linux (`Backend::snapshot()`), against the FF3 ground truth

    fn truth_context(name: &str) -> serde_json::Value {
        let path = format!(
            "{}/tests/fixtures/apps/truth.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let truth: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        truth["contexts"][name].clone()
    }

    fn recorded(name: &str, title: &str) -> ThreadContext {
        let path = format!(
            "{}/tests/fixtures/trees/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let snap = Snapshot::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(snap.focused().is_some_and(|f| f.states.focused), "{name}");
        let app = AppInfo {
            id: Some("google-chrome".into()),
            name: "Google Chrome".into(),
            window_title: Some(format!("{title} - Google Chrome")),
            url: None,
        };
        extract(&snap, &app, "Draft:", &ThreadLimits::default())
    }

    fn collapse(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Every truth message found, in order, nothing else: same text (whitespace collapsed),
    /// the author (an address may follow it), and `from_me` for the user's messages.
    fn assert_matches_truth(ctx: &ThreadContext, truth: &serde_json::Value) {
        let want = truth["messages"].as_array().unwrap();
        let got: Vec<_> = ctx
            .messages
            .iter()
            .map(|m| (m.author.as_deref(), collapse(&m.text), m.from_me))
            .collect();
        assert_eq!(got.len(), want.len(), "{got:#?}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!(g.1, collapse(w["text"].as_str().unwrap()));
            let author = w["author"].as_str().unwrap();
            assert!(
                g.0.is_some_and(|a| a == author || a.starts_with(&format!("{author} <"))),
                "{g:?} by {author}"
            );
            if w["from_me"].as_bool().unwrap() {
                assert_eq!(g.2, Some(true), "{g:?}");
            } else {
                assert_ne!(g.2, Some(true), "{g:?}");
            }
        }
    }

    #[test]
    fn recorded_chrome_linux_chat_matches_truth() {
        let ctx = recorded("web_chrome_linux_chat", "Nugget fixture: Plain textarea");
        let truth = truth_context("chat");
        assert_matches_truth(&ctx, &truth);
        assert_eq!(ctx.messages[1].timestamp.as_deref(), Some("10:03 AM"));
        assert!(ctx.recipients.is_empty());
        assert_eq!(ctx.subject, None);
    }

    #[test]
    fn recorded_chrome_linux_mail_matches_truth() {
        let ctx = recorded("web_chrome_linux_mail", "Nugget fixture: Mail compose");
        let truth = truth_context("mail");
        assert_matches_truth(&ctx, &truth);
        assert_eq!(
            ctx.messages[2].timestamp.as_deref(),
            Some("Tue, Oct 27, 8:05 AM")
        );
        assert_eq!(ctx.recipients, vec!["Jordan Lee", "Priya Natarajan"]);
        assert_eq!(ctx.subject.as_deref(), Some("Re: Q3 offsite venue"));
    }

    #[test]
    fn forms_and_pickers_are_never_a_thread() {
        let item = |t: &str| {
            let mut n = Node::new(Role::ListItem);
            n.children = vec![static_text(t, [0.0, 0.0, 300.0, 18.0])];
            n
        };
        let mut form = Node::new(Role::Group);
        form.children = vec![
            item("Re-enter password to send"),
            item("Password for this account"),
        ];
        assert!(score(&form, None, true, false) > 0.0);
        let mut pw = Node::new(Role::TextField);
        pw.states.secure = true;
        form.children.push(pw);
        assert_eq!(score(&form, None, true, false), 0.0);
        let mut chips = Node::new(Role::List);
        chips.children = vec![
            item("Jordan Lee, the venue person"),
            item("Priya Natarajan"),
        ];
        assert!(score(&chips, None, true, false) > 0.0);
        chips.attrs.insert("xml_roles".into(), "listbox".into());
        assert_eq!(score(&chips, None, true, false), 0.0);
    }
    #[test]
    fn generic_grouping_follows_the_column() {
        let draft = |author: Option<&str>, x: f64| Draft {
            author: author.map(str::to_string),
            text: "text".into(),
            indent: Some(x),
            ..Default::default()
        };
        let mut drafts = vec![
            draft(Some("Maya Chen"), 60.0),
            draft(None, 60.0),
            draft(None, 420.0),
        ];
        group_by_author(&mut drafts, AppFamily::Generic);
        let authors: Vec<_> = drafts.iter().map(|d| d.author.as_deref()).collect();
        // A continuation in the same column joins; a bubble in the other column does not.
        assert_eq!(authors, [Some("Maya Chen"), Some("Maya Chen"), None]);
        // Without geometry, an authorless message is not grouped in a generic app.
        let mut drafts = vec![
            Draft {
                indent: None,
                ..draft(Some("Maya Chen"), 0.0)
            },
            Draft {
                indent: None,
                ..draft(None, 0.0)
            },
        ];
        group_by_author(&mut drafts, AppFamily::Generic);
        assert_eq!(drafts[1].author, None);
        // Slack groups regardless of the column.
        let mut drafts = vec![draft(Some("Maya Chen"), 60.0), draft(None, 420.0)];
        group_by_author(&mut drafts, AppFamily::Slack);
        assert_eq!(drafts[1].author.as_deref(), Some("Maya Chen"));
    }
}
