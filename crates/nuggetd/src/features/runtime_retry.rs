//! `runtime.retry` (KK1): the user's Retry after a local model server failed to start, crashed
//! or hung until the supervisor gave up. Clears the failure and starts the server again in the
//! background; `runtime.status` shows it `starting` (warming up), then `ready` or `failed`.
//!
//! Params: [`RuntimeRetryRequest`] (`{role}`). Result: `{ "started": bool }` (`false` when the
//! role has no installed model to start).

use std::sync::Arc;

use async_trait::async_trait;
use nugget_core::Result;
use nugget_core::ipc::RuntimeRetryRequest;
use nugget_ipc::{Notifier, RpcError, to_result};
use serde_json::{Value, json};

use super::{Feature, FeatureContext};
use crate::daemon::{State, spawn_ensure};

use nugget_core::ipc::runtime_retry_method::RETRY;

pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    Ok(Arc::new(RuntimeRetry {
        state: cx.state.clone(),
    }))
}

struct RuntimeRetry {
    state: Arc<State>,
}

#[async_trait]
impl Feature for RuntimeRetry {
    fn name(&self) -> &'static str {
        "runtime_retry"
    }

    fn methods(&self) -> &'static [&'static str] {
        &[RETRY]
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        _notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        if method != RETRY {
            return Err(RpcError::method_not_found(method));
        }
        let req: RuntimeRetryRequest = serde_json::from_value(params)
            .map_err(|e| RpcError::invalid_params(format!("runtime.retry: {e}")))?;
        let Some(local) = self.state.local.as_ref() else {
            return to_result(json!({ "started": false }));
        };
        let Some(slot) = local.slots.iter().find(|s| s.role() == req.role) else {
            return to_result(json!({ "started": false }));
        };
        if !slot.is_installed() {
            return to_result(json!({ "started": false }));
        }
        tracing::info!(provider = %slot.id(), "local runtime retry requested");
        local.runtime.retry(req.role);
        spawn_ensure(&self.state, slot.clone());
        to_result(json!({ "started": true }))
    }
}
