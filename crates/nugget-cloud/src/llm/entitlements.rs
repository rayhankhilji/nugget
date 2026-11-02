//! What a user may do with the model endpoint, read through ONE function ([`entitlements`]).
//!
//! Billing (plans, trials, add-ons) is being added separately; when it lands, only the body of
//! [`entitlements`] (and [`Entitlements::for_plan`]) changes. The endpoint never looks at
//! `profiles.plan` or a quota itself.

use uuid::Uuid;

use crate::db::{Db, DbError};
use crate::gateway::Plan;

/// Limits and model access for one user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entitlements {
    pub plan: Plan,
    /// Monthly AI budget in micro-USD (LLM and embeddings).
    pub quota_micros: i64,
    /// Whether `heed-smart` gets the smart model; otherwise it is served by the fast model.
    pub smart_model: bool,
    /// Largest `max_tokens` honoured per request, by tier (larger values are lowered).
    pub max_output_fast: u32,
    pub max_output_smart: u32,
    /// Streams one user may have open at once.
    pub max_concurrent: usize,
}

impl Entitlements {
    pub fn for_plan(plan: Plan) -> Self {
        match plan {
            Plan::Free => Self {
                plan,
                quota_micros: plan.quota_micros(),
                smart_model: false,
                max_output_fast: 4_096,
                max_output_smart: 8_192,
                max_concurrent: 4,
            },
            Plan::Pro => Self {
                plan,
                quota_micros: plan.quota_micros(),
                smart_model: true,
                max_output_fast: 4_096,
                max_output_smart: 32_000,
                max_concurrent: 8,
            },
        }
    }
}

/// The user's entitlements. Also marks them active (as every authenticated route does).
pub async fn entitlements(db: &Db, user: Uuid) -> Result<Entitlements, DbError> {
    let plan = Plan::parse(&db.touch_profile(user).await?);
    Ok(Entitlements::for_plan(plan))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_differ_where_it_matters() {
        let free = Entitlements::for_plan(Plan::Free);
        let pro = Entitlements::for_plan(Plan::Pro);
        assert!(!free.smart_model && pro.smart_model);
        assert!(pro.quota_micros > free.quota_micros);
        assert_eq!(free.quota_micros, Plan::Free.quota_micros());
    }
}
