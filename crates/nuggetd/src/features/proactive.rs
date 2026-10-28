//! Proactive cards and the morning brief (W4). Skeleton; filled in by the daemon work.

mod exec;

use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::Result;
use nugget_ipc::{Notifier, RpcError};
use serde_json::Value;

use super::{Feature, FeatureContext};

pub(crate) async fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let _ = cx;
    Ok(Arc::new(Proactive))
}

struct Proactive;

#[async_trait]
impl Feature for Proactive {
    fn name(&self) -> &'static str {
        "proactive"
    }
    fn methods(&self) -> &'static [&'static str] {
        &[]
    }
    async fn call(&self, method: &str, _: Value, _: Notifier) -> Result<Value, RpcError> {
        Err(RpcError::method_not_found(method))
    }
}
