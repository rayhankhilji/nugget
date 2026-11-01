//! Chat imports: iMessage (`imessage`, the macOS Messages database), WhatsApp "Export chat"
//! files (`whatsapp`) and Telegram Desktop JSON exports (`telegram`).
//!
//! ## Items
//! - **One document per conversation per calendar month (UTC)** that has messages.
//! - **Key**: `<conversation id>/<YYYY-MM>`; the conversation id is `im:<chat guid>`,
//!   `wa:<slug of the chat name>` or `tg:<chat id>`. A month larger than
//!   [`MAX_DOC_BYTES`] is split into `<id>/<YYYY-MM>`, `<id>/<YYYY-MM>.2`, ...
//! - **Content**: Markdown ([`Content::Text`](nugget_connector::Content::Text), `text/markdown`):
//!   YAML frontmatter (`type: conversation`, service, conversation, participants, period), a
//!   heading, then one block per message. Message text is untrusted: every line is quoted with
//!   `> `, names are put on one line and escaped, frontmatter values are YAML-quoted. System
//!   lines ("Alice added Bob") are rendered as `_time · text_`. A message over
//!   [`MAX_MESSAGE_BYTES`] is truncated with a note.
//! - **Version**: SHA-256 (hex) of the rendered document and its metadata, so it is stable
//!   across runs and changes when either changes (e.g. toggling `private`).
//! - **Metadata**: `service`, `thread_id`, `conversation`, `is_group`, `period`,
//!   `message_count`, `first_at`, `last_at`, `participants` ({name, handle, me}), `contacts`
//!   ({name, phones, emails}, for the knowledge contacts extractor), `attachments`
//!   ({name, mime, size}; attachment content is never read) and `sensitive` (the `private`
//!   setting, default on: keeps the chats away from cloud models and cloud embeddings).
//! - **URL**: `file://` URL of the export (WhatsApp, Telegram); none for iMessage.
//!
//! ## Sync
//! Every pass is a reconciling full pass: read the source, render every document, upsert those
//! whose version differs from [`KnownItems`](nugget_connector::KnownItems), delete known keys no
//! longer produced, and checkpoint `{"v":1}` every [`CHECKPOINT_EVERY`] sends. `since_days`
//! drops older messages, `include_groups: false` drops group chats. A WhatsApp export that
//! cannot be read (half-written, corrupt) keeps its previously imported documents; an
//! unreadable Telegram export keeps everything. Several exports of the same chat are merged and
//! duplicate messages (same time, author and text) dropped, so re-exporting a chat updates it.
//!
//! iMessage copies `chat.db` (+ `-wal`, `-shm`) into a private temp dir under the cache dir and
//! reads the copy (never the live database); contacts come from the macOS address book when
//! readable. Permission errors on the database (macOS privacy protection) become
//! `Error::Unauthorized` asking for Full Disk Access. It polls every 120 s.
//!
//! ## Watch
//! WhatsApp and Telegram watch their folder / file with a native debounced watcher; any change
//! runs a reconciling pass (without checkpoints). They poll every 300 s otherwise.

mod connector;
mod imessage;
mod model;
mod render;
mod settings;
mod telegram;
pub mod typedstream;
mod whatsapp;

use std::sync::Arc;

use nugget_connector::{
    AuthKind, Connector, ConnectorContext, ConnectorFactory, KindInfo, Registry,
};
use nugget_core::Result;
use serde_json::Value;

pub use connector::CHECKPOINT_EVERY;
pub use render::{MAX_DOC_BYTES, MAX_MESSAGE_BYTES};
pub use settings::{
    FULL_DISK_ACCESS_MESSAGE, IMessageSettings, TelegramSettings, WhatsAppSettings,
    imessage_schema, telegram_schema, whatsapp_schema,
};
pub use whatsapp::{MAX_CHAT_TEXT_BYTES, chat_name};

use connector::MessagingConnector;

/// Kind name of the iMessage connector.
pub const IMESSAGE_KIND: &str = "imessage";
/// Kind name of the WhatsApp exports connector.
pub const WHATSAPP_KIND: &str = "whatsapp";
/// Kind name of the Telegram export connector.
pub const TELEGRAM_KIND: &str = "telegram";

/// Factory for the `imessage` kind.
#[derive(Debug, Default, Clone, Copy)]
pub struct IMessageFactory;

impl ConnectorFactory for IMessageFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: IMESSAGE_KIND,
            display_name: "iMessage",
            auth: AuthKind::None,
            settings_schema: imessage_schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        let s = IMessageSettings::parse(settings)?;
        settings::check_db_readable(&s.db_path()?)
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let s = IMessageSettings::parse(settings)?;
        let reader = imessage::IMessageReader::new(
            s.db_path()?,
            s.contacts_dir(),
            ctx.paths.cache.join("messaging"),
        );
        Ok(Arc::new(MessagingConnector::new(
            Arc::new(reader),
            s.common(),
            ctx,
        )))
    }
}

/// Factory for the `whatsapp` kind.
#[derive(Debug, Default, Clone, Copy)]
pub struct WhatsAppFactory;

impl ConnectorFactory for WhatsAppFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: WHATSAPP_KIND,
            display_name: "WhatsApp exports",
            auth: AuthKind::None,
            settings_schema: whatsapp_schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        let s = WhatsAppSettings::parse(settings)?;
        settings::check_folder(&s.folder(), "WhatsApp export folder")
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let s = WhatsAppSettings::parse(settings)?;
        let reader = whatsapp::WhatsAppReader {
            folder: s.folder(),
            me: s.me.clone().filter(|m| !m.trim().is_empty()),
        };
        Ok(Arc::new(MessagingConnector::new(
            Arc::new(reader),
            s.common(),
            ctx,
        )))
    }
}

/// Factory for the `telegram` kind.
#[derive(Debug, Default, Clone, Copy)]
pub struct TelegramFactory;

impl ConnectorFactory for TelegramFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: TELEGRAM_KIND,
            display_name: "Telegram export",
            auth: AuthKind::None,
            settings_schema: telegram_schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        let s = TelegramSettings::parse(settings)?;
        settings::check_exists(&s.path(), "Telegram export")
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        let s = TelegramSettings::parse(settings)?;
        let reader = telegram::TelegramReader { path: s.path() };
        Ok(Arc::new(MessagingConnector::new(
            Arc::new(reader),
            s.common(),
            ctx,
        )))
    }
}

/// Register the three kinds.
pub fn register(registry: &mut Registry) {
    registry.register(Arc::new(IMessageFactory));
    registry.register(Arc::new(WhatsAppFactory));
    registry.register(Arc::new(TelegramFactory));
}
