//! Stripe: customers, Checkout, the customer portal, subscription lookups, and webhook
//! signature checks. Form-encoded REST at api.stripe.com with the secret key.

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use time::OffsetDateTime;

#[derive(Debug, thiserror::Error)]
pub enum StripeError {
    #[error("Stripe is not configured (set STRIPE_SECRET_KEY)")]
    NotConfigured,
    #[error("Stripe request failed: {0}")]
    Http(String),
}

/// What we keep of a Stripe subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub id: String,
    pub customer: String,
    pub status: String,
    pub price_id: String,
    pub trial_end: Option<OffsetDateTime>,
    pub current_period_end: Option<OffsetDateTime>,
    pub cancel_at_period_end: bool,
    /// `metadata.user_id` set at checkout, if present.
    pub user_id: Option<String>,
}

pub struct CheckoutRequest<'a> {
    pub customer: &'a str,
    pub price: &'a str,
    pub user_id: &'a str,
    /// None = no trial (the user had one before).
    pub trial_days: Option<u32>,
    pub success_url: &'a str,
    pub cancel_url: &'a str,
}

#[async_trait]
pub trait Stripe: Send + Sync {
    async fn create_customer(
        &self,
        email: Option<&str>,
        user_id: &str,
    ) -> Result<String, StripeError>;
    /// Returns the Checkout URL.
    async fn checkout(&self, req: &CheckoutRequest<'_>) -> Result<String, StripeError>;
    /// Returns the customer-portal URL.
    async fn portal(&self, customer: &str, return_url: &str) -> Result<String, StripeError>;
    async fn subscription(&self, id: &str) -> Result<Subscription, StripeError>;
    /// Deletes the customer, which cancels their subscriptions at once. Invoices stay in Stripe
    /// (kept there for HMRC's six years); a missing customer counts as deleted.
    async fn delete_customer(&self, id: &str) -> Result<(), StripeError>;
}

fn ts(v: &Value) -> Option<OffsetDateTime> {
    v.as_i64()
        .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
}

/// Parses a subscription object. `current_period_end` moved onto subscription items in newer
/// API versions; both places are read.
pub fn parse_subscription(v: &Value) -> Option<Subscription> {
    let item = &v["items"]["data"][0];
    Some(Subscription {
        id: v["id"].as_str()?.to_owned(),
        customer: match &v["customer"] {
            Value::String(s) => s.clone(),
            o => o["id"].as_str()?.to_owned(),
        },
        status: v["status"].as_str()?.to_owned(),
        price_id: item["price"]["id"].as_str()?.to_owned(),
        trial_end: ts(&v["trial_end"]),
        current_period_end: ts(&v["current_period_end"])
            .or_else(|| ts(&item["current_period_end"])),
        cancel_at_period_end: v["cancel_at_period_end"].as_bool().unwrap_or(false),
        user_id: v["metadata"]["user_id"].as_str().map(str::to_owned),
    })
}

pub struct HttpStripe {
    pub http: reqwest::Client,
    pub base: String,
    pub key: String,
}

impl HttpStripe {
    async fn call(
        &self,
        req: reqwest::RequestBuilder,
        idempotency: Option<&str>,
    ) -> Result<Value, StripeError> {
        let mut req = req.bearer_auth(&self.key);
        if let Some(k) = idempotency {
            req = req.header("Idempotency-Key", k);
        }
        let res = req
            .send()
            .await
            .map_err(|e| StripeError::Http(e.to_string()))?;
        let status = res.status();
        let v: Value = res
            .json()
            .await
            .map_err(|e| StripeError::Http(e.to_string()))?;
        if !status.is_success() {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            return Err(StripeError::Http(format!("{status}: {msg}")));
        }
        Ok(v)
    }

    async fn post(
        &self,
        path: &str,
        form: &[(&str, String)],
        idempotency: Option<&str>,
    ) -> Result<Value, StripeError> {
        self.call(
            self.http.post(format!("{}{path}", self.base)).form(form),
            idempotency,
        )
        .await
    }
}

fn field(v: &Value, k: &str) -> Result<String, StripeError> {
    v[k].as_str()
        .map(str::to_owned)
        .ok_or_else(|| StripeError::Http(format!("response without {k}")))
}

#[async_trait]
impl Stripe for HttpStripe {
    async fn create_customer(
        &self,
        email: Option<&str>,
        user_id: &str,
    ) -> Result<String, StripeError> {
        let mut form = vec![("metadata[user_id]", user_id.to_owned())];
        if let Some(e) = email {
            form.push(("email", e.to_owned()));
        }
        // One customer per user even if two requests race.
        let v = self
            .post("/v1/customers", &form, Some(&format!("customer-{user_id}")))
            .await?;
        field(&v, "id")
    }

    async fn checkout(&self, r: &CheckoutRequest<'_>) -> Result<String, StripeError> {
        let mut form = vec![
            ("mode", "subscription".to_owned()),
            ("customer", r.customer.to_owned()),
            ("client_reference_id", r.user_id.to_owned()),
            ("line_items[0][price]", r.price.to_owned()),
            ("line_items[0][quantity]", "1".to_owned()),
            ("success_url", r.success_url.to_owned()),
            ("cancel_url", r.cancel_url.to_owned()),
            ("payment_method_collection", "always".to_owned()),
            ("subscription_data[metadata][user_id]", r.user_id.to_owned()),
            ("allow_promotion_codes", "true".to_owned()),
        ];
        if let Some(d) = r.trial_days.filter(|d| *d > 0) {
            form.push(("subscription_data[trial_period_days]", d.to_string()));
        }
        field(
            &self.post("/v1/checkout/sessions", &form, None).await?,
            "url",
        )
    }

    async fn portal(&self, customer: &str, return_url: &str) -> Result<String, StripeError> {
        let form = [
            ("customer", customer.to_owned()),
            ("return_url", return_url.to_owned()),
        ];
        field(
            &self
                .post("/v1/billing_portal/sessions", &form, None)
                .await?,
            "url",
        )
    }

    async fn subscription(&self, id: &str) -> Result<Subscription, StripeError> {
        if !id.starts_with("sub_") || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(StripeError::Http("bad subscription id".into()));
        }
        let v = self
            .call(
                self.http
                    .get(format!("{}/v1/subscriptions/{id}", self.base)),
                None,
            )
            .await?;
        parse_subscription(&v)
            .ok_or_else(|| StripeError::Http("unexpected subscription shape".into()))
    }

    async fn delete_customer(&self, id: &str) -> Result<(), StripeError> {
        if !id.starts_with("cus_") || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(StripeError::Http("bad customer id".into()));
        }
        let res = self
            .http
            .delete(format!("{}/v1/customers/{id}", self.base))
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(|e| StripeError::Http(e.to_string()))?;
        match res.status().as_u16() {
            200..=299 | 404 => Ok(()),
            s => Err(StripeError::Http(format!("delete customer: status {s}"))),
        }
    }
}

/// Checks a `Stripe-Signature` header (`t=…,v1=…[,v1=…]`): HMAC-SHA256 of `{t}.{body}` with the
/// endpoint secret, hex, within `tolerance` seconds of `now`.
pub fn verify_signature(secret: &str, header: &str, body: &[u8], now: i64, tolerance: i64) -> bool {
    let mut t = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_owned()),
            _ => {}
        }
    }
    let Some(t) = t else { return false };
    if (now - t).abs() > tolerance {
        return false;
    }
    sigs.iter().any(|s| {
        let Ok(expected) = hex::decode(s) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(t.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        mac.verify_slice(&expected).is_ok()
    })
}

#[cfg(test)]
pub fn sign(secret: &str, t: i64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{t}.").as_bytes());
    mac.update(body);
    format!("t={t},v1={}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn signatures() {
        let body = br#"{"id":"evt_1"}"#;
        let h = sign("whsec_x", 1000, body);
        assert!(verify_signature("whsec_x", &h, body, 1100, 300));
        assert!(
            verify_signature(
                "whsec_x",
                &format!("t=1000,v1=00,{}", &h[7..]),
                body,
                1100,
                300
            ),
            "any v1"
        );
        assert!(!verify_signature("whsec_y", &h, body, 1100, 300));
        assert!(!verify_signature("whsec_x", &h, b"{}", 1100, 300));
        assert!(!verify_signature("whsec_x", &h, body, 2000, 300), "stale");
        assert!(
            !verify_signature("whsec_x", "v1=abc", body, 1000, 300),
            "no timestamp"
        );
    }

    #[test]
    fn subscription_shapes() {
        let old = json!({"id": "sub_1", "customer": "cus_1", "status": "trialing", "trial_end": 1800000000,
            "current_period_end": 1800000000, "cancel_at_period_end": false, "metadata": {"user_id": "u"},
            "items": {"data": [{"price": {"id": "price_1"}}]}});
        let s = parse_subscription(&old).unwrap();
        assert_eq!(
            (s.status.as_str(), s.price_id.as_str(), s.user_id.as_deref()),
            ("trialing", "price_1", Some("u"))
        );
        assert_eq!(
            s.current_period_end.unwrap().unix_timestamp(),
            1_800_000_000
        );
        let new = json!({"id": "sub_2", "customer": {"id": "cus_2"}, "status": "active", "cancel_at_period_end": true,
            "items": {"data": [{"price": {"id": "price_2"}, "current_period_end": 1900000000}]}});
        let s = parse_subscription(&new).unwrap();
        assert_eq!(s.customer, "cus_2");
        assert!(s.cancel_at_period_end);
        assert_eq!(
            s.current_period_end.unwrap().unix_timestamp(),
            1_900_000_000
        );
        assert!(parse_subscription(&json!({"id": "sub_3"})).is_none());
    }

    #[tokio::test]
    async fn checkout_and_portal_requests() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/checkout/sessions"))
            .and(header("authorization", "Bearer sk_test"))
            .and(body_string_contains("mode=subscription"))
            .and(body_string_contains(
                "subscription_data%5Btrial_period_days%5D=14",
            ))
            .and(body_string_contains("line_items%5B0%5D%5Bprice%5D=price_1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "cs_1", "url": "https://checkout.stripe.com/c/1"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/billing_portal/sessions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"url": "https://billing.stripe.com/p/1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/customers"))
            .and(header("idempotency-key", "customer-u1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "cus_9"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/subscriptions/sub_1"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({"error": {"message": "No such subscription"}})),
            )
            .mount(&server)
            .await;
        let s = HttpStripe {
            http: reqwest::Client::new(),
            base: server.uri(),
            key: "sk_test".into(),
        };
        let url = s
            .checkout(&CheckoutRequest {
                customer: "cus_1",
                price: "price_1",
                user_id: "u1",
                trial_days: Some(14),
                success_url: "https://api/billing/done",
                cancel_url: "https://api/billing/done",
            })
            .await
            .unwrap();
        assert_eq!(url, "https://checkout.stripe.com/c/1");
        assert_eq!(
            s.portal("cus_1", "https://api/billing/done").await.unwrap(),
            "https://billing.stripe.com/p/1"
        );
        assert_eq!(s.create_customer(Some("a@b"), "u1").await.unwrap(), "cus_9");
        assert!(
            s.subscription("sub_1")
                .await
                .unwrap_err()
                .to_string()
                .contains("No such subscription")
        );
        assert!(s.subscription("../x").await.is_err());
    }
}
