//! Plans and entitlements from `plans.toml` (embedded; `NUGGET_PLANS_FILE` overrides).
//! There is no free plan: without a live subscription a user has no entitlement.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const DEFAULT_TOML: &str = include_str!("../plans.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Fast,
    Smart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interval {
    #[serde(rename = "month")]
    Month,
    #[serde(rename = "year")]
    Year,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub name: String,
    pub monthly_gbp: u32,
    pub annual_gbp: u32,
    pub stripe_price_monthly_env: String,
    pub stripe_price_annual_env: String,
    pub default_model: ModelTier,
    pub smart_asks_per_month: u32,
    pub ai_budget_usd: f64,
    pub ai_hard_cap_usd: f64,
    /// None = unlimited.
    pub max_connections: Option<u32>,
    pub sync_interval_minutes: u32,
    /// None = all history.
    pub history_days: Option<u32>,
    pub devices: u32,
    pub ghost_text: bool,
    pub needs_you_inbox: bool,
    pub daily_brief: bool,
    pub actions_with_approval: bool,
    pub agent_tasks: bool,
    pub computer_use: bool,
    pub priority_support: bool,
}

impl Plan {
    pub fn ai_budget_micros(&self) -> i64 {
        (self.ai_budget_usd * 1e6) as i64
    }
    pub fn ai_hard_cap_micros(&self) -> i64 {
        (self.ai_hard_cap_usd * 1e6) as i64
    }
    pub fn price_env(&self, interval: Interval) -> &str {
        match interval {
            Interval::Month => &self.stripe_price_monthly_env,
            Interval::Year => &self.stripe_price_annual_env,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub trial_days: u32,
    pub plans: BTreeMap<String, Plan>,
    /// Stripe price id → (plan id, interval), filled from the environment.
    #[serde(skip)]
    prices: BTreeMap<String, (String, Interval)>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlansError {
    #[error("plans.toml: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("plans.toml: {0}")]
    Invalid(String),
}

impl Catalog {
    /// Parses and validates; resolves Stripe price ids with `env`.
    pub fn parse(text: &str, env: impl Fn(&str) -> Option<String>) -> Result<Self, PlansError> {
        let mut c: Catalog = toml::from_str(text)?;
        let bad = |m: String| Err(PlansError::Invalid(m));
        if c.plans.is_empty() {
            return bad("no plans".into());
        }
        if c.trial_days > 730 {
            return bad("trial_days too long".into());
        }
        for (id, p) in &c.plans {
            if !id.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_') {
                return bad(format!("plan id {id:?} must be lowercase letters and _"));
            }
            if !(p.ai_budget_usd > 0.0 && p.ai_hard_cap_usd >= p.ai_budget_usd) {
                return bad(format!("{id}: need 0 < ai_budget_usd <= ai_hard_cap_usd"));
            }
            if p.sync_interval_minutes == 0 || p.devices == 0 {
                return bad(format!(
                    "{id}: sync_interval_minutes and devices must be at least 1"
                ));
            }
            for interval in [Interval::Month, Interval::Year] {
                if let Some(price) = env(p.price_env(interval)).filter(|v| !v.trim().is_empty())
                    && c.prices
                        .insert(price.trim().to_owned(), (id.clone(), interval))
                        .is_some()
                {
                    return bad(format!("Stripe price {price} is used twice"));
                }
            }
        }
        Ok(c)
    }

    pub fn load(env: impl Fn(&str) -> Option<String>) -> Result<Self, PlansError> {
        match env("NUGGET_PLANS_FILE") {
            Some(path) => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| PlansError::Invalid(format!("{path}: {e}")))?;
                Self::parse(&text, env)
            }
            None => Self::parse(DEFAULT_TOML, env),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Plan> {
        self.plans.get(id)
    }

    /// The Stripe price id for a plan and interval, if configured.
    pub fn price_id(&self, plan: &str, interval: Interval) -> Option<&str> {
        self.prices
            .iter()
            .find(|(_, (p, i))| p == plan && *i == interval)
            .map(|(k, _)| k.as_str())
    }

    /// Which plan and interval a Stripe price id stands for.
    pub fn by_price(&self, price: &str) -> Option<(&str, Interval)> {
        self.prices.get(price).map(|(p, i)| (p.as_str(), *i))
    }
}

/// Subscription statuses that grant the plan. `past_due` keeps access while Stripe retries the
/// payment (its dunning ends in `canceled` or `unpaid`, which do not).
pub fn grants_access(status: &str) -> bool {
    matches!(status, "active" | "trialing" | "past_due")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(k: &str) -> Option<String> {
        match k {
            "STRIPE_PRICE_HEED_MONTHLY" => Some("price_hm".into()),
            "STRIPE_PRICE_HEED_ANNUAL" => Some("price_ha".into()),
            "STRIPE_PRICE_HEED_MAX_MONTHLY" => Some("price_xm".into()),
            _ => None,
        }
    }

    #[test]
    fn defaults_parse_and_match_the_offer() {
        let c = Catalog::parse(DEFAULT_TOML, env).unwrap();
        assert_eq!(c.trial_days, 14);
        let heed = c.get("heed").unwrap();
        let max = c.get("heed_max").unwrap();
        assert_eq!((heed.monthly_gbp, heed.annual_gbp), (39, 390));
        assert_eq!((max.monthly_gbp, max.annual_gbp), (99, 990));
        assert_eq!(heed.annual_gbp, heed.monthly_gbp * 10);
        assert_eq!(heed.default_model, ModelTier::Fast);
        assert_eq!(heed.smart_asks_per_month, 300);
        assert_eq!(
            (
                heed.max_connections,
                heed.sync_interval_minutes,
                heed.history_days,
                heed.devices
            ),
            (Some(10), 30, Some(365), 1)
        );
        assert_eq!(
            (
                max.max_connections,
                max.sync_interval_minutes,
                max.history_days,
                max.devices
            ),
            (None, 5, None, 3)
        );
        assert!(max.agent_tasks && max.computer_use && max.priority_support);
        assert!(!heed.agent_tasks && heed.actions_with_approval && heed.ghost_text);
        assert_eq!(c.price_id("heed", Interval::Year), Some("price_ha"));
        assert_eq!(c.price_id("heed_max", Interval::Year), None, "unset env");
        assert_eq!(c.by_price("price_xm"), Some(("heed_max", Interval::Month)));
    }

    #[test]
    fn invalid_files_are_refused() {
        let ok = DEFAULT_TOML;
        assert!(
            Catalog::parse(
                &ok.replace("ai_hard_cap_usd = 12", "ai_hard_cap_usd = 1"),
                env
            )
            .is_err()
        );
        assert!(Catalog::parse(&ok.replace("devices = 1", "devices = 0"), env).is_err());
        assert!(
            Catalog::parse(
                &ok.replace("ghost_text = true\nneeds", "ghost_txt = true\nneeds"),
                env
            )
            .is_err(),
            "typos are caught"
        );
        assert!(Catalog::parse("trial_days = 1\n[plans]\n", env).is_err());
        let dup = |k: &str| {
            if k.starts_with("STRIPE_PRICE") {
                Some("same".into())
            } else {
                None
            }
        };
        assert!(Catalog::parse(ok, dup).is_err());
    }

    #[test]
    fn access_by_status() {
        for s in ["active", "trialing", "past_due"] {
            assert!(grants_access(s));
        }
        for s in [
            "canceled",
            "unpaid",
            "incomplete",
            "incomplete_expired",
            "paused",
        ] {
            assert!(!grants_access(s));
        }
    }
}
