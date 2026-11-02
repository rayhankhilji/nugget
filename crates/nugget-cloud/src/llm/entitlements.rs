//! What a user may do with the model endpoint, read through ONE function ([`entitlements`]),
//! itself a view of billing's single lookup ([`crate::plans::entitlement`]). The endpoint never
//! reads subscriptions or `plans.toml` itself.

use uuid::Uuid;

use crate::db::{Db, DbError};
use crate::gateway::Allowance;
use crate::plans::{Catalog, ModelTier, Plan};

/// Limits and model access for one user with a live subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct Entitlements {
    pub plan_id: String,
    /// Fair-use budget, hard cap, smart allowance and default model (as the gateway's).
    pub allowance: Allowance,
    /// Largest `max_tokens` honoured per request, by tier (larger values are lowered).
    pub max_output_fast: u32,
    pub max_output_smart: u32,
    /// Streams one user may have open at once.
    pub max_concurrent: usize,
}

impl Entitlements {
    pub fn for_plan(plan_id: &str, plan: &Plan) -> Self {
        let smart_first = plan.default_model == ModelTier::Smart;
        Self {
            plan_id: plan_id.to_owned(),
            allowance: Allowance::from(plan),
            max_output_fast: 4_096,
            max_output_smart: if smart_first { 32_000 } else { 8_192 },
            // A few streams per device (ghost text cancels its own; asks and the agent overlap).
            max_concurrent: 4 * plan.devices.max(1) as usize,
        }
    }
}

/// The user's entitlements; `None` without a live subscription (the endpoint answers 402).
pub async fn entitlements(
    db: &Db,
    catalog: &Catalog,
    user: Uuid,
) -> Result<Option<Entitlements>, DbError> {
    Ok(crate::plans::entitlement(db, catalog, user)
        .await?
        .map(|e| Entitlements::for_plan(&e.plan_id, &e.plan)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_differ_where_it_matters() {
        let c = Catalog::parse(crate::plans::DEFAULT_TOML, crate::testkit::test_prices).unwrap();
        let heed = Entitlements::for_plan("heed", c.get("heed").unwrap());
        let max = Entitlements::for_plan("heed_max", c.get("heed_max").unwrap());
        assert_eq!(heed.allowance.default_model, ModelTier::Fast);
        assert_eq!(max.allowance.default_model, ModelTier::Smart);
        assert!(max.allowance.hard_cap_micros > heed.allowance.hard_cap_micros);
        assert!(max.max_concurrent > heed.max_concurrent);
        assert!(max.max_output_smart > heed.max_output_smart);
    }
}
