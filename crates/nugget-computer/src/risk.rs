//! Risk of one computer-use call, from the element it targets (looked up in the last snapshot,
//! never on the OS). The table is in the crate docs; the rules here only ever raise risk, and an
//! element that is not in the cache is treated as one that sends (the fail-safe forces a
//! confirmation).

use nugget_a11y::computer::{Key, KeyChord, NamedKey};
use nugget_a11y::role::Role;
use nugget_a11y::{UiActionKind, UiNode};
use nugget_policy::RiskClass;

use crate::config::RiskKeywords;
use crate::render::is_text_input;

/// Words on a control that mean the click sends something to someone.
pub const SEND_WORDS: &[&str] = &[
    "send", "submit", "post", "publish", "share", "invite", "tweet",
];
/// Words that mean the click removes or overwrites data.
pub const DESTROY_WORDS: &[&str] = &[
    "delete",
    "remove",
    "discard",
    "erase",
    "archive",
    "move to trash",
    "move to bin",
    "empty trash",
    "empty bin",
    "unsubscribe",
    "cancel subscription",
    "cancel event",
    "cancel meeting",
    "cancel appointment",
    "cancel booking",
    "cancel reservation",
    "cancel order",
    "wipe",
    "overwrite",
    "replace all",
];
/// Words that mean the click spends money or commits to something.
pub const SPEND_WORDS: &[&str] = &[
    "pay",
    "buy",
    "purchase",
    "place order",
    "book",
    "subscribe",
    "donate",
    "transfer",
    "sign",
    "order now",
    "confirm purchase",
];
/// Words that mean the click changes security or system state.
pub const SECURITY_WORDS: &[&str] = &[
    "password",
    "passcode",
    "passkey",
    "permission",
    "security",
    "install",
    "uninstall",
    "allow",
    "grant",
    "2fa",
    "two factor",
    "authorize",
    "authenticate",
    "trust",
    // Protections a toggle can switch off (DD3: turning off the screen lock ran unconfirmed).
    "lock screen",
    "screen lock",
    "screen locks",
    "auto lock",
    "firewall",
    "remote access",
    "remote login",
    "encryption",
    "antivirus",
];
/// Buttons that confirm whatever their dialog asks; they take the dialog's risk.
const AFFIRMATIVE_WORDS: &[&str] = &[
    "ok", "okay", "yes", "continue", "confirm", "proceed", "accept", "agree", "apply", "done",
    "next", "finish", "go",
];
/// Words in a field's label that mean typed text is a secret.
const SECRET_FIELD_WORDS: &[&str] = &[
    "password",
    "passcode",
    "passphrase",
    "passkey",
    "pin",
    "otp",
    "secret",
    "token",
    "cvv",
    "cvc",
    "security code",
    "verification code",
    "2fa",
    "ssn",
    "api key",
];

/// Terminal emulators, by a word of their name or app id. Text typed into a terminal can run
/// any command, so `type_text`, `press_key` and `set_value` there are Security risk.
pub const TERMINAL_APPS: &[&str] = &[
    "terminal",
    "windowsterminal",
    "iterm",
    "iterm2",
    "conhost",
    "cmd",
    "cmd.exe",
    "powershell",
    "pwsh",
    "konsole",
    "xterm",
    "uxterm",
    "rxvt",
    "urxvt",
    "alacritty",
    "kitty",
    "wezterm",
    "wezterm-gui",
    "foot",
    "footclient",
    "tilix",
    "terminator",
    "terminology",
    "guake",
    "yakuake",
    "tilda",
    "warp",
    "ghostty",
    "hyper",
    "tabby",
    "contour",
    "blackbox",
    "ptyxis",
];

/// Is an app with this name or id a terminal emulator? `extra` extends [`TERMINAL_APPS`].
pub fn is_terminal_app(name_or_id: &str, extra: &[String]) -> bool {
    let w = words(name_or_id);
    TERMINAL_APPS
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
        .any(|t| {
            let t = t.trim();
            !t.is_empty() && (name_or_id.trim().eq_ignore_ascii_case(t) || has_phrase(&w, t))
        })
}

/// Does `role` (a raw platform role) name a terminal widget?
pub fn is_terminal_role(raw_role: &str) -> bool {
    raw_role.to_ascii_lowercase().contains("terminal")
}

/// Lowercase alphanumeric words. `sign in`/`sign up`/`sign out`/`log in` become one word so
/// that "sign" (a signature) does not match them.
fn words(text: &str) -> Vec<String> {
    let raw: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let next = raw.get(i + 1).map(String::as_str);
        if matches!(raw[i].as_str(), "sign" | "log")
            && matches!(next, Some("in" | "up" | "out" | "on" | "into"))
        {
            out.push(format!("{}_{}", raw[i], next.unwrap_or_default()));
            i += 2;
        } else {
            out.push(raw[i].clone());
            i += 1;
        }
    }
    out
}

fn has_phrase(text_words: &[String], phrase: &str) -> bool {
    let p = words(phrase);
    if p.is_empty() || p.len() > text_words.len() {
        return false;
    }
    if p.len() == 1 {
        let k = &p[0];
        return text_words
            .iter()
            .any(|w| w == k || w.strip_suffix('s') == Some(k.as_str()));
    }
    text_words.windows(p.len()).any(|w| w == p.as_slice())
}

fn any_phrase<'a>(text_words: &[String], lists: impl IntoIterator<Item = &'a str>) -> bool {
    lists.into_iter().any(|p| has_phrase(text_words, p))
}

/// The highest class whose words appear in `text`, if any.
pub fn classify_text(text: &str, extra: &RiskKeywords) -> Option<RiskClass> {
    let w = words(text);
    if w.is_empty() {
        return None;
    }
    let lists: [(RiskClass, &[&str], &[String]); 4] = [
        (RiskClass::Security, SECURITY_WORDS, &extra.security),
        (RiskClass::Spend, SPEND_WORDS, &extra.spend),
        (RiskClass::Destroy, DESTROY_WORDS, &extra.destroy),
        (RiskClass::Send, SEND_WORDS, &extra.send),
    ];
    lists.into_iter().find_map(|(class, builtin, extra)| {
        let hit = any_phrase(&w, builtin.iter().copied())
            || any_phrase(&w, extra.iter().map(String::as_str));
        hit.then_some(class)
    })
}

/// Text that labels an element: name, description, placeholder.
pub(crate) fn label_text(node: &UiNode) -> String {
    [&node.name, &node.description, &node.placeholder]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Is `node` a field whose contents are secret (secure, or labelled like a password)?
pub fn is_secret_field(node: &UiNode) -> bool {
    node.states.secure
        || any_phrase(
            &words(&label_text(node)),
            SECRET_FIELD_WORDS.iter().copied(),
        )
}

fn is_affirmative(node: &UiNode) -> bool {
    let w = words(node.name.as_deref().unwrap_or_default());
    !w.is_empty() && w.len() <= 3 && any_phrase(&w, AFFIRMATIVE_WORDS.iter().copied())
}

/// Containers whose removals are reversible: a shopping cart and its translations. Matched
/// against the target's ancestors' names, its dialog's text and the window title.
pub const CART_WORDS: &[&str] = &[
    "cart",
    "shopping cart",
    "basket",
    "shopping basket",
    "trolley",
    "shopping bag",
    "your bag",
    "my bag",
    "warenkorb",
    "einkaufswagen",
    "panier",
    "carrito",
    "cesta",
    "carrello",
    "winkelwagen",
    "winkelmand",
    "carrinho",
    "kundvagn",
    "koszyk",
];
/// Verbs a cart line's own removal button uses.
const CART_REMOVAL_WORDS: &[&str] = &["remove", "delete"];
/// Objects whose removal is not a cart line even inside a cart: these stay Destroy.
const CART_PROTECTED_WORDS: &[&str] = &[
    "account",
    "payment",
    "card",
    "method",
    "wallet",
    "address",
    "profile",
    "all",
    "everything",
    "history",
    "data",
    "order",
    "subscription",
];
/// Labels of buttons that submit or commit their form (a sign-in, a send, a payment).
const SUBMIT_WORDS: &[&str] = &[
    "submit",
    "send",
    "post",
    "publish",
    "pay",
    "place order",
    "purchase",
    "buy",
    "buy now",
    "order now",
    "confirm purchase",
    "checkout",
    "check out",
    "sign in",
    "log in",
    "sign up",
    "save",
];

/// Does a container name, dialog text or window title name a shopping cart?
fn names_cart(text: &str) -> bool {
    let w = words(text);
    any_phrase(&w, CART_WORDS.iter().copied()) || (w.len() == 1 && w[0] == "bag")
}

/// Is the target inside a cart: an ancestor, its dialog or the window title names one?
fn in_cart(ancestors: &[(Role, Option<String>)], context: &[String]) -> bool {
    ancestors
        .iter()
        .filter_map(|(_, name)| name.as_deref())
        .chain(context.iter().map(String::as_str))
        .any(names_cart)
}

/// Is `text` the removal of one cart line ("Remove", "Remove one Gel pen", "Delete",
/// "Remove from cart")? Only remove/delete may carry its risk: with those words dropped, no
/// other risk word may be left, nor a protected object ("Remove payment method").
fn is_cart_line_removal(text: &str, kw: &RiskKeywords) -> bool {
    let w = words(text);
    if !any_phrase(&w, CART_REMOVAL_WORDS.iter().copied())
        || any_phrase(&w, CART_PROTECTED_WORDS.iter().copied())
    {
        return false;
    }
    let rest: Vec<&str> = w
        .iter()
        .map(String::as_str)
        .filter(|x| !CART_REMOVAL_WORDS.contains(x))
        .collect();
    classify_text(&rest.join(" "), kw).is_none()
}

/// What a click on `node` risks. `context` is the text around it (its dialog's title and text,
/// the window title), consulted for confirming buttons such as "OK" in "Delete 3 files?".
/// `None` (the element is not in the last snapshot) is treated as [`RiskClass::Send`].
pub fn click_risk(node: Option<&UiNode>, context: &[String], kw: &RiskKeywords) -> RiskClass {
    click_risk_in(node, &[], context, kw)
}

/// [`click_risk`] for an element looked up in the last snapshot, with its containers.
pub(crate) fn click_risk_found(
    found: Option<&crate::state::Found>,
    kw: &RiskKeywords,
) -> RiskClass {
    match found {
        Some(f) => click_risk_in(Some(&f.node), &f.ancestors, &f.context, kw),
        None => click_risk_in(None, &[], &[], kw),
    }
}

/// [`click_risk`] knowing the element's containers (innermost first, with their names). Inside
/// a shopping cart ([`CART_WORDS`] in a container's name, the dialog or the window title),
/// removing one line ("Remove", "Remove one Gel pen", "Delete") is Edit: the item can be put
/// back. Any other Destroy label ("Remove payment method", "Empty trash") and every Security,
/// Spend or Send word keep their class.
pub fn click_risk_in(
    node: Option<&UiNode>,
    ancestors: &[(Role, Option<String>)],
    context: &[String],
    kw: &RiskKeywords,
) -> RiskClass {
    let Some(node) = node else {
        return RiskClass::Send;
    };
    let label = label_text(node);
    let mut own = classify_text(&label, kw);
    // Toggling a check box or switch sets a preference ("Send me the newsletter"); it sends or
    // spends nothing by itself. Destroy and Security words still count.
    if (node.role == Role::CheckBox || node.states.checked.is_some())
        && matches!(own, Some(RiskClass::Send | RiskClass::Spend))
    {
        own = Some(RiskClass::Edit);
    }
    let cart = || in_cart(ancestors, context);
    if own == Some(RiskClass::Destroy) && is_cart_line_removal(&label, kw) && cart() {
        own = Some(RiskClass::Edit);
    }
    let ctx = if is_affirmative(node) {
        let text = context.join(" ");
        match classify_text(&text, kw) {
            // "OK" in "Remove Gel pen from your cart?"
            Some(RiskClass::Destroy) if is_cart_line_removal(&text, kw) && cart() => {
                Some(RiskClass::Edit)
            }
            c => c,
        }
    } else {
        None
    };
    own.into_iter()
        .chain(ctx)
        .max()
        .unwrap_or(RiskClass::Edit)
        .max(RiskClass::Edit)
}

/// Does activating `node` submit what its form or dialog holds? `Some(true)` for a button
/// labelled like a submit ("Send", "Pay", "Place order", "Sign in"; "OK"/"Continue" in a
/// dialog), `Some(false)` for controls that never submit (check boxes, list items, links,
/// tabs, menu items, text inputs), `None` when it cannot be told. The accessibility model
/// exposes no default-button flag, so this goes by role and label.
fn submits(node: &UiNode, in_dialog: bool) -> Option<bool> {
    if node.role == Role::CheckBox
        || node.states.checked.is_some()
        || is_text_input(node)
        || matches!(
            node.role,
            Role::ListItem | Role::Link | Role::Tab | Role::MenuItem
        )
    {
        return Some(false);
    }
    if node.role != Role::Button {
        return None;
    }
    let w = words(node.name.as_deref().unwrap_or_default());
    if w.is_empty() || w.len() > 5 {
        return None;
    }
    if any_phrase(&w, SUBMIT_WORDS.iter().copied()) || (in_dialog && is_affirmative(node)) {
        return Some(true);
    }
    None
}

/// What the policy gate should know about the element a call targets: its role, containers,
/// sibling fields, dialog and whether it submits. `None` (not in the last snapshot) is an empty
/// context.
pub(crate) fn action_context(found: Option<&crate::state::Found>) -> nugget_policy::ActionContext {
    let Some(f) = found else {
        return nugget_policy::ActionContext::default();
    };
    let in_dialog = f.ancestors.iter().any(|(r, _)| *r == Role::Dialog);
    let mut ctx = nugget_policy::ActionContext::default()
        .with_role(f.node.role.as_str())
        .with_ancestors(
            f.ancestors
                .iter()
                .map(|(role, name)| nugget_policy::ContextNode::new(role.as_str(), name.clone())),
        )
        .with_form_fields(f.fields.iter().cloned())
        .with_secure_field(is_secret_field(&f.node));
    if let Some(s) = submits(&f.node, in_dialog) {
        ctx = ctx.with_submits(s);
    }
    if in_dialog {
        // `context` is the dialog's name, description and texts, then the window title.
        let n = f.context.len() - usize::from(f.window.title.is_some()).min(f.context.len());
        let text = f.context[..n].join("\n");
        if !text.is_empty() {
            ctx = ctx.with_dialog_text(text);
        }
    }
    if let Some(t) = &f.window.title {
        ctx = ctx.with_window_title(t.clone());
    }
    ctx
}

/// What typing into (or setting the value of) `node` risks: a secret field is Security, an
/// unknown element Send, anything else Edit.
pub fn input_risk(node: Option<&UiNode>, explicit_id: bool) -> RiskClass {
    match node {
        Some(n) if is_secret_field(n) => RiskClass::Security,
        Some(_) => RiskClass::Edit,
        None if explicit_id => RiskClass::Send,
        None => RiskClass::Edit,
    }
}

fn is_multiline(node: &UiNode) -> bool {
    matches!(node.role, Role::TextArea | Role::Document) || node.states.multiline == Some(true)
}

/// What pressing `chords` with focus on `target` risks (`None`: focus unknown). See the crate
/// docs for the table.
pub fn keys_risk(
    chords: &[KeyChord],
    target: Option<&UiNode>,
    context: &[String],
    kw: &RiskKeywords,
) -> RiskClass {
    let pressable = target.is_some_and(|n| {
        n.actions.contains(&UiActionKind::Press) || matches!(n.role, Role::Button | Role::Link)
    });
    let text_input = target.is_some_and(is_text_input);
    chords
        .iter()
        .map(|c| {
            let m = c.modifiers;
            match c.key {
                Key::Named(NamedKey::Enter) => {
                    if m.ctrl || m.meta {
                        RiskClass::Send
                    } else if target.is_some_and(is_multiline) {
                        RiskClass::Edit
                    } else if pressable {
                        click_risk(target, context, kw).max(RiskClass::Send)
                    } else {
                        RiskClass::Send
                    }
                }
                Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                    if m.shift || m.meta || !text_input {
                        RiskClass::Destroy
                    } else {
                        RiskClass::Edit
                    }
                }
                Key::Named(NamedKey::Space) if !text_input => match target {
                    None => RiskClass::Send,
                    Some(_) if pressable => click_risk(target, context, kw),
                    Some(_) => RiskClass::Edit,
                },
                Key::Named(NamedKey::F(4)) if m.alt => RiskClass::Destroy,
                Key::Char(ch) if ch.eq_ignore_ascii_case(&'q') && (m.ctrl || m.meta) => {
                    RiskClass::Destroy
                }
                _ => RiskClass::Edit,
            }
        })
        .max()
        .unwrap_or(RiskClass::Edit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Found;

    fn kw() -> RiskKeywords {
        RiskKeywords::default()
    }

    #[test]
    fn text_classes() {
        let k = kw();
        assert_eq!(classify_text("Send", &k), Some(RiskClass::Send));
        assert_eq!(classify_text("Reply All", &k), None);
        assert_eq!(classify_text("Delete draft", &k), Some(RiskClass::Destroy));
        assert_eq!(classify_text("Empty Trash…", &k), Some(RiskClass::Destroy));
        assert_eq!(classify_text("Place order", &k), Some(RiskClass::Spend));
        assert_eq!(classify_text("Sign document", &k), Some(RiskClass::Spend));
        assert_eq!(classify_text("Sign in", &k), None);
        assert_eq!(classify_text("Design", &k), None);
        assert_eq!(classify_text("Postpone", &k), None);
        assert_eq!(
            classify_text("Change password", &k),
            Some(RiskClass::Security)
        );
        assert_eq!(classify_text("Unsubscribe", &k), Some(RiskClass::Destroy));
        assert_eq!(classify_text("Subscribe", &k), Some(RiskClass::Spend));
        assert_eq!(classify_text("Permissions", &k), Some(RiskClass::Security));
        assert_eq!(
            classify_text("Lock screen after 5 minutes", &k),
            Some(RiskClass::Security)
        );
        assert_eq!(
            classify_text("Remote access", &k),
            Some(RiskClass::Security)
        );
        assert_eq!(classify_text("Lock", &k), None);
        // The highest class wins.
        assert_eq!(
            classify_text("Delete and send", &k),
            Some(RiskClass::Destroy)
        );
        let extra = RiskKeywords {
            send: vec!["dispatch now".into()],
            ..RiskKeywords::default()
        };
        assert_eq!(classify_text("Dispatch now", &extra), Some(RiskClass::Send));
    }

    #[test]
    fn unknown_click_is_send() {
        assert_eq!(click_risk(None, &[], &kw()), RiskClass::Send);
    }

    #[test]
    fn affirmative_button_takes_dialog_risk() {
        let ok = UiNode {
            role: Role::Button,
            name: Some("OK".into()),
            ..UiNode::default()
        };
        let ctx = vec!["Delete 3 files?".to_string()];
        assert_eq!(click_risk(Some(&ok), &ctx, &kw()), RiskClass::Destroy);
        assert_eq!(click_risk(Some(&ok), &[], &kw()), RiskClass::Edit);
        let other = UiNode {
            role: Role::Button,
            name: Some("Bold".into()),
            ..UiNode::default()
        };
        assert_eq!(click_risk(Some(&other), &ctx, &kw()), RiskClass::Edit);
    }

    #[test]
    fn keys() {
        let p = |s: &str| KeyChord::parse_list(s).unwrap();
        let field = UiNode {
            role: Role::TextField,
            states: nugget_a11y::tree::States {
                editable: true,
                ..Default::default()
            },
            ..UiNode::default()
        };
        let area = UiNode {
            role: Role::TextArea,
            ..field.clone()
        };
        let k = kw();
        assert_eq!(
            keys_risk(&p("enter"), Some(&field), &[], &k),
            RiskClass::Send
        );
        assert_eq!(
            keys_risk(&p("enter"), Some(&area), &[], &k),
            RiskClass::Edit
        );
        assert_eq!(
            keys_risk(&p("ctrl+enter"), Some(&area), &[], &k),
            RiskClass::Send
        );
        assert_eq!(
            keys_risk(&p("cmd+enter"), Some(&area), &[], &k),
            RiskClass::Send
        );
        assert_eq!(keys_risk(&p("enter"), None, &[], &k), RiskClass::Send);
        assert_eq!(
            keys_risk(&p("ctrl+a"), Some(&field), &[], &k),
            RiskClass::Edit
        );
        assert_eq!(
            keys_risk(&p("backspace"), Some(&field), &[], &k),
            RiskClass::Edit
        );
        assert_eq!(
            keys_risk(&p("shift+delete"), Some(&field), &[], &k),
            RiskClass::Destroy
        );
        assert_eq!(keys_risk(&p("delete"), None, &[], &k), RiskClass::Destroy);
        assert_eq!(keys_risk(&p("tab tab"), None, &[], &k), RiskClass::Edit);
        assert_eq!(keys_risk(&p("cmd+q"), None, &[], &k), RiskClass::Destroy);
        assert_eq!(keys_risk(&p("space"), None, &[], &k), RiskClass::Send);
    }

    #[test]
    fn terminals() {
        for t in [
            "Terminal",
            "com.apple.Terminal",
            "iTerm2",
            "com.googlecode.iterm2",
            "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
            "Windows Terminal",
            "cmd.exe",
            "powershell",
            "org.gnome.Terminal",
            "gnome-terminal-server",
            "konsole",
            "xterm",
            "Alacritty",
            "kitty",
            "org.wezfurlong.wezterm",
            "foot",
            "xfce4-terminal",
            "dev.warp.Warp-Stable",
            "com.mitchellh.ghostty",
            "Hyper",
        ] {
            assert!(is_terminal_app(t, &[]), "{t}");
        }
        for t in [
            "Mail",
            "Football Manager",
            "Notes",
            "com.apple.mail",
            "Hyperlink Tool 2",
        ] {
            // Whole words only: "Hyperlink" is not "hyper", "Football" is not "foot".
            assert!(!is_terminal_app(t, &[]), "{t}");
        }
        assert!(is_terminal_app("MyShell", &["myshell".into()]));
        assert!(is_terminal_role("terminal"));
        assert!(is_terminal_role("AXTerminal"));
    }

    #[test]
    fn toggles_and_draft_openers_are_edits() {
        let kw = RiskKeywords::default();
        let mut cb = UiNode {
            role: Role::CheckBox,
            name: Some("Send me the city newsletter".into()),
            ..UiNode::default()
        };
        assert_eq!(click_risk(Some(&cb), &[], &kw), RiskClass::Edit);
        cb.name = Some("Delete my data after 30 days".into());
        assert_eq!(click_risk(Some(&cb), &[], &kw), RiskClass::Destroy);
        for name in ["Reply", "Forward", "Checkout"] {
            let b = UiNode {
                role: Role::Button,
                name: Some(name.into()),
                ..UiNode::default()
            };
            assert_eq!(click_risk(Some(&b), &[], &kw), RiskClass::Edit, "{name}");
        }
        let send = UiNode {
            role: Role::Button,
            name: Some("Send".into()),
            ..UiNode::default()
        };
        assert_eq!(click_risk(Some(&send), &[], &kw), RiskClass::Send);
    }

    fn button(name: &str) -> UiNode {
        UiNode {
            role: Role::Button,
            name: Some(name.into()),
            ..UiNode::default()
        }
    }

    fn cart_line() -> Vec<(Role, Option<String>)> {
        vec![
            (Role::ListItem, None),
            (Role::List, Some("Cart".into())),
            (Role::Window, Some("Corner Shop".into())),
        ]
    }

    #[test]
    fn cart_line_removal_is_edit() {
        let k = kw();
        let shop = vec!["Corner Shop".to_string()];
        for name in [
            "Remove one Gel pen",
            "Remove",
            "Remove Gel pen",
            "Delete",
            "Remove from cart",
            "Decrease quantity",
        ] {
            assert_eq!(
                click_risk_in(Some(&button(name)), &cart_line(), &shop, &k),
                RiskClass::Edit,
                "{name}"
            );
        }
        // A cart named by the window title, or in another language.
        let basket = vec!["Your shopping basket".to_string()];
        assert_eq!(
            click_risk_in(Some(&button("Remove")), &[], &basket, &k),
            RiskClass::Edit
        );
        let de = vec![(Role::List, Some("Warenkorb".into()))];
        assert_eq!(
            click_risk_in(Some(&button("Delete")), &de, &[], &k),
            RiskClass::Edit
        );
        // "OK" in a dialog asking to remove a cart line.
        let dlg = vec![
            "Remove Gel pen from your cart?".to_string(),
            "Corner Shop".to_string(),
        ];
        assert_eq!(
            click_risk_in(Some(&button("OK")), &[], &dlg, &k),
            RiskClass::Edit
        );
    }

    #[test]
    fn removal_outside_cart_or_of_protected_things_stays() {
        let k = kw();
        let shop = vec!["Corner Shop".to_string()];
        let files = vec![
            (Role::ListItem, None),
            (Role::List, Some("Files".into())),
            (Role::Window, Some("Corner Shop".into())),
        ];
        assert_eq!(
            click_risk_in(Some(&button("Remove one Gel pen")), &files, &shop, &k),
            RiskClass::Destroy
        );
        assert_eq!(
            click_risk_in(Some(&button("Remove")), &[], &[], &k),
            RiskClass::Destroy
        );
        assert_eq!(
            click_risk(Some(&button("Remove one Gel pen")), &shop, &k),
            RiskClass::Destroy
        );
        for (name, class) in [
            ("Remove payment method", RiskClass::Destroy),
            ("Delete account", RiskClass::Destroy),
            ("Remove card", RiskClass::Destroy),
            ("Empty trash", RiskClass::Destroy),
            ("Remove all", RiskClass::Destroy),
            ("Cancel order", RiskClass::Destroy),
            ("Remove permission", RiskClass::Security),
            ("Remove and buy", RiskClass::Spend),
        ] {
            assert_eq!(
                click_risk_in(Some(&button(name)), &cart_line(), &shop, &k),
                class,
                "{name}"
            );
        }
        // A list of "Tea bags" is not a bag.
        let tea = vec![(Role::List, Some("Tea bags".into()))];
        assert_eq!(
            click_risk_in(Some(&button("Remove")), &tea, &[], &k),
            RiskClass::Destroy
        );
        // "OK" in a dialog deleting something else, inside a shop window.
        let dlg = vec!["Delete your account?".to_string(), "Cart".to_string()];
        assert_eq!(
            click_risk_in(Some(&button("OK")), &[], &dlg, &k),
            RiskClass::Destroy
        );
    }

    fn found(node: UiNode, ancestors: Vec<(Role, Option<String>)>, context: Vec<String>) -> Found {
        Found {
            node,
            window: nugget_a11y::computer::WindowInfo {
                id: nugget_a11y::computer::WindowId("w1".into()),
                app: nugget_a11y::computer::AppEntry {
                    id: None,
                    name: "Shop".into(),
                    pid: None,
                    frontmost: true,
                },
                title: Some("Corner Shop".into()),
                bounds: None,
                focused: true,
                minimized: false,
            },
            app: "Shop".into(),
            context,
            ancestors,
            fields: vec!["Email".into(), "Card number".into()],
        }
    }

    #[test]
    fn found_click_risk_and_context() {
        let k = kw();
        let f = found(
            button("Remove one Gel pen"),
            cart_line(),
            vec!["Corner Shop".into()],
        );
        assert_eq!(click_risk_found(Some(&f), &k), RiskClass::Edit);
        assert_eq!(click_risk_found(None, &k), RiskClass::Send);
        let c = action_context(Some(&f));
        assert_eq!(c.role.as_deref(), Some("button"));
        assert_eq!(c.ancestors.len(), 3);
        assert_eq!(c.ancestors[1].role, "list");
        assert_eq!(c.ancestors[1].name.as_deref(), Some("Cart"));
        assert_eq!(c.form_fields, vec!["Email", "Card number"]);
        assert_eq!(c.window_title.as_deref(), Some("Corner Shop"));
        assert_eq!(c.submits, None);
        assert_eq!(c.dialog_text, None);

        let pay = found(button("Pay $12.40"), vec![], vec!["Corner Shop".into()]);
        assert_eq!(action_context(Some(&pay)).submits, Some(true));
        let ok = found(
            button("OK"),
            vec![(Role::Dialog, Some("Leave?".into()))],
            vec![
                "Leave?".into(),
                "Your cart is kept.".into(),
                "Corner Shop".into(),
            ],
        );
        let c = action_context(Some(&ok));
        assert_eq!(c.submits, Some(true));
        assert_eq!(c.dialog_text.as_deref(), Some("Leave?\nYour cart is kept."));
        // "OK" outside a dialog: cannot tell.
        let ok_bare = found(button("OK"), vec![], vec![]);
        assert_eq!(action_context(Some(&ok_bare)).submits, None);
        for role in [
            Role::CheckBox,
            Role::ListItem,
            Role::Link,
            Role::Tab,
            Role::MenuItem,
        ] {
            let n = UiNode {
                role,
                name: Some("Send".into()),
                ..UiNode::default()
            };
            let f = found(n, vec![], vec![]);
            assert_eq!(action_context(Some(&f)).submits, Some(false), "{role:?}");
        }
        assert_eq!(
            action_context(None),
            nugget_policy::ActionContext::default()
        );
    }

    #[test]
    fn secret_fields() {
        let mut n = UiNode {
            role: Role::TextField,
            name: Some("Password".into()),
            ..UiNode::default()
        };
        assert!(is_secret_field(&n));
        assert_eq!(input_risk(Some(&n), true), RiskClass::Security);
        n.name = Some("Subject".into());
        assert!(!is_secret_field(&n));
        assert_eq!(input_risk(Some(&n), true), RiskClass::Edit);
        assert_eq!(input_risk(None, true), RiskClass::Send);
        assert_eq!(input_risk(None, false), RiskClass::Edit);
    }
}
