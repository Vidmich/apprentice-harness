//! The cost formula. Prices are USD per million tokens, so
//! `tokens × price` is already micro-dollars:
//!
//! ```text
//! cost_micros = round(input          * pricing.input
//!                   + output         * pricing.output
//!                   + cache_read     * pricing.cache_read
//!                   + cache_creation * pricing.cache_write)
//! ```
//!
//! `input_tokens` from the API excludes cached tokens, so the four terms are
//! additive. A model without a pricing entry has no cost (`None`), which the
//! aggregates surface as `unpriced_calls`.

use std::collections::BTreeMap;

use apprentice_api::types::Usage;

use crate::config::Pricing;

/// Prices keyed by model id (`[pricing.<model>]` in config).
pub type PriceTable = BTreeMap<String, Pricing>;

/// Cost of `usage` at `pricing`, in micro-dollars, rounded half away from
/// zero.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "token counts are far below 2^53 and the rounded sum fits i64"
)]
pub fn cost_micros(usage: &Usage, pricing: &Pricing) -> i64 {
    let usd_micros = usage.input_tokens as f64 * pricing.input
        + usage.output_tokens as f64 * pricing.output
        + usage.cache_read_input_tokens as f64 * pricing.cache_read()
        + usage.cache_creation_input_tokens as f64 * pricing.cache_write();
    usd_micros.round() as i64
}

/// Cost of one call, or `None` when `model` has no pricing entry.
pub fn price_call(model: &str, usage: &Usage, table: &PriceTable) -> Option<i64> {
    table.get(model).map(|p| cost_micros(usage, p))
}

/// Micro-dollars as a float for display and the wire (`cost_usd`).
#[expect(
    clippy::cast_precision_loss,
    reason = "costs stay far below 2^53 micro-dollars"
)]
pub fn micros_to_usd(micros: i64) -> f64 {
    micros as f64 / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(i: u64, o: u64, cr: u64, cw: u64) -> Usage {
        Usage {
            input_tokens: i,
            output_tokens: o,
            cache_read_input_tokens: cr,
            cache_creation_input_tokens: cw,
        }
    }

    #[test]
    fn known_usage_times_known_pricing() {
        // Opus 5: $5 in, $25 out, $0.50 cache read, $6.25 cache write.
        let p = Pricing::first_party(5.0, 25.0);
        assert_eq!(cost_micros(&usage(1000, 100, 0, 0), &p), 5_000 + 2_500);
        assert_eq!(
            cost_micros(&usage(1_204, 310, 2_048, 28), &p),
            6_020 + 7_750 + 1_024 + 175
        );
        assert_eq!(cost_micros(&Usage::default(), &p), 0);
    }

    #[test]
    fn rounds_half_away_from_zero() {
        let p = Pricing {
            input: 0.25,
            output: 0.0,
            cache_read: Some(0.0),
            cache_write: Some(0.0),
        };
        assert_eq!(cost_micros(&usage(1, 0, 0, 0), &p), 0); // 0.25
        assert_eq!(cost_micros(&usage(2, 0, 0, 0), &p), 1); // 0.5
        assert_eq!(cost_micros(&usage(3, 0, 0, 0), &p), 1); // 0.75
        assert_eq!(cost_micros(&usage(6, 0, 0, 0), &p), 2); // 1.5
    }

    #[test]
    fn explicit_cache_prices_override_the_convention() {
        let p = Pricing {
            input: 1.0,
            output: 1.0,
            cache_read: Some(0.3),
            cache_write: None,
        };
        assert_eq!(cost_micros(&usage(0, 0, 10, 8), &p), 3 + 10);
    }

    #[test]
    fn unpriced_model_has_no_cost() {
        let table: PriceTable = [("m".to_owned(), Pricing::first_party(1.0, 2.0))].into();
        assert_eq!(price_call("m", &usage(10, 10, 0, 0), &table), Some(30));
        assert_eq!(price_call("other", &usage(10, 10, 0, 0), &table), None);
    }

    #[test]
    fn micros_convert_to_dollars() {
        assert!((micros_to_usd(1_840_000) - 1.84).abs() < 1e-12);
        assert!((micros_to_usd(0)).abs() < f64::EPSILON);
    }
}
