//! Token accounting (task M00-07): every mentor call's `usage` becomes a
//! cost at record time (`mentor_calls.cost_micros`, see [`price_call`]) and
//! is aggregated per model, day, session, workspace and kind for
//! `stats.tokens`; `stats.calls` lists the calls behind the sums (task
//! M01-13). Prices come from config (`[pricing.<model>]`);
//! [`StatsService::reprice`] recomputes stored costs after a pricing
//! correction.
//!
//! Only API-reported usage is counted; nothing is estimated locally.

mod cost;
mod range;
mod rpc;

pub use cost::{PriceTable, cost_micros, micros_to_usd, price_call, usd_to_micros};
pub use range::{BoundKind, format_offset, parse_bound};
pub use rpc::{PricingSource, StatsService, token_stats};
