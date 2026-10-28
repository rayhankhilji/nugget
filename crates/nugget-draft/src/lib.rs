//! Double Tap drafting engine (daemon side).
//!
//! **Contract (M3):** [`FieldKind`] and [`classify`]. Work package M3-C builds the engine: prompt
//! assembly from the voice profile, the thread and budgeted, cited retrieval context; streaming
//! generation with post-processing; the cloud/sensitive gate. Field kinds are decided here by
//! rules, never by the model.

use nugget_core::ipc::DraftContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    EmailBody,
    ChatMessage,
    Comment,
    Document,
    FormField,
    Search,
    Unknown,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmailBody => "email_body",
            Self::ChatMessage => "chat_message",
            Self::Comment => "comment",
            Self::Document => "document",
            Self::FormField => "form_field",
            Self::Search => "search",
            Self::Unknown => "unknown",
        }
    }

    /// Whether drafting a reply makes sense for this kind.
    pub fn draftable(self) -> bool {
        !matches!(self, Self::Search)
    }
}

/// Decide what kind of field this is from app, role, labels and thread (minimal stub; M3-C
/// replaces it with the full rule set).
pub fn classify(ctx: &DraftContext) -> FieldKind {
    if ctx.field.role == "search_field" {
        return FieldKind::Search;
    }
    if ctx.subject.is_some() || !ctx.recipients.is_empty() {
        return FieldKind::EmailBody;
    }
    if !ctx.thread.is_empty() {
        return FieldKind::ChatMessage;
    }
    FieldKind::Unknown
}
