//! `stats.tokens` and `stats.reprice` over the trace store and the pricing
//! table. The daemon registers them on its router (task M00-08).

use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    StatsReprice, StatsRepriceParams, StatsRepriceResult, StatsTokens, StatsTokensParams,
};
use apprentice_api::server::{Connection, Router};
use apprentice_api::types::{ApprenticeStats, StatsRange, TokenBucket, TokenStats};
use time::{OffsetDateTime, UtcOffset};

use super::cost::{PriceTable, micros_to_usd, price_call};
use super::range::{BoundKind, format_offset, parse_bound};
use crate::config::ConfigLoader;
use crate::trace::{CallFilter, GroupBy, GroupedTotals, TraceError, TraceStore, UsageTotals};

/// Where the price table comes from.
#[derive(Debug, Clone)]
pub enum PricingSource {
    /// `[pricing]` of the resolved user config, loaded per request so edits
    /// apply without a restart.
    Config(ConfigLoader),
    /// A fixed table (tests, embedding).
    Fixed(PriceTable),
}

/// Handlers for `stats.tokens` and `stats.reprice`.
#[derive(Debug, Clone)]
pub struct StatsService {
    store: Arc<TraceStore>,
    pricing: PricingSource,
    /// `None` = ask the store for the OS local offset on each request.
    offset: Option<UtcOffset>,
}

impl StatsService {
    pub fn new(store: Arc<TraceStore>, loader: ConfigLoader) -> Self {
        Self {
            store,
            pricing: PricingSource::Config(loader),
            offset: None,
        }
    }

    pub fn with_pricing(store: Arc<TraceStore>, table: PriceTable) -> Self {
        Self {
            store,
            pricing: PricingSource::Fixed(table),
            offset: None,
        }
    }

    /// Fixes the timezone used for `by_day` and bare dates (tests).
    #[must_use]
    pub fn with_offset(mut self, offset: UtcOffset) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn store(&self) -> &Arc<TraceStore> {
        &self.store
    }

    /// The current price table.
    ///
    /// # Errors
    /// Config error (-32050) when the user config cannot be read.
    pub fn pricing(&self) -> Result<PriceTable, RpcError> {
        match &self.pricing {
            PricingSource::Config(loader) => Ok(loader.load(None)?.config.pricing),
            PricingSource::Fixed(table) => Ok(table.clone()),
        }
    }

    fn offset(&self) -> Result<UtcOffset, TraceError> {
        match self.offset {
            Some(o) => Ok(o),
            None => Ok(
                UtcOffset::from_whole_seconds(self.store.local_offset_secs()?)
                    .unwrap_or(UtcOffset::UTC),
            ),
        }
    }

    fn filter(
        since: Option<&str>,
        until: Option<&str>,
        session_id: Option<&str>,
        model: Option<&str>,
        offset: UtcOffset,
    ) -> Result<CallFilter, RpcError> {
        let now = OffsetDateTime::now_utc();
        let bound = |text: Option<&str>, kind| {
            text.map(|t| parse_bound(t, kind, now, offset))
                .transpose()
                .map_err(RpcError::invalid_params)
        };
        Ok(CallFilter {
            session_id: session_id.map(Into::into),
            agent_id: None,
            model: model.map(str::to_owned),
            status: None,
            since: bound(since, BoundKind::Since)?,
            until: bound(until, BoundKind::Until)?,
        })
    }

    /// Aggregates over calls matching the params. `by_session` is only
    /// filled when the query is not already limited to one session.
    ///
    /// # Errors
    /// `invalid_params` for an unparsable bound, else a store error.
    pub fn tokens(&self, p: &StatsTokensParams) -> Result<TokenStats, RpcError> {
        let offset = self.offset()?;
        let filter = Self::filter(
            p.since.as_deref(),
            p.until.as_deref(),
            p.session_id.as_deref(),
            None,
            offset,
        )?;
        Ok(token_stats(&self.store, &filter, offset)?)
    }

    /// Recomputes stored costs from the current price table.
    ///
    /// # Errors
    /// `invalid_params` for an unparsable bound, config or store errors.
    pub fn reprice(&self, p: &StatsRepriceParams) -> Result<StatsRepriceResult, RpcError> {
        let table = self.pricing()?;
        let offset = self.offset()?;
        let filter = Self::filter(
            p.since.as_deref(),
            p.until.as_deref(),
            p.session_id.as_deref(),
            p.model.as_deref(),
            offset,
        )?;
        let report = self
            .store
            .reprice(&filter, &|model, usage| price_call(model, usage, &table))?;
        Ok(StatsRepriceResult {
            examined: report.examined,
            changed: report.changed,
            unpriced: report.unpriced,
        })
    }

    /// Registers both methods; each runs on the blocking pool.
    pub fn register(self: Arc<Self>, router: &mut Router) {
        let svc = Arc::clone(&self);
        router.add::<StatsTokens, _, _>(move |_c: Arc<Connection>, p: StatsTokensParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.tokens(&p))
        });
        let svc = Arc::clone(&self);
        router.add::<StatsReprice, _, _>(move |_c: Arc<Connection>, p: StatsRepriceParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.reprice(&p))
        });
    }
}

/// Builds the wire result: totals plus per-model, per-day (in `offset`) and
/// per-session groups. The range echoes the resolved bounds.
///
/// # Errors
/// Store errors.
pub fn token_stats(
    store: &TraceStore,
    filter: &CallFilter,
    offset: UtcOffset,
) -> Result<TokenStats, TraceError> {
    let totals = store.stats(filter)?;
    let by_model = store.stats_by(filter, GroupBy::Model)?;
    let by_day = store.stats_by(
        filter,
        GroupBy::Day {
            offset_secs: offset.whole_seconds(),
        },
    )?;
    let by_session = if filter.session_id.is_some() {
        Vec::new()
    } else {
        store.stats_by(filter, GroupBy::Session)?
    };
    Ok(TokenStats {
        range: StatsRange {
            since: filter.since.clone(),
            until: filter.until.clone(),
        },
        tz: format_offset(offset),
        totals: bucket(None, None, &totals),
        by_model: by_model.iter().map(grouped).collect(),
        by_day: by_day.iter().map(grouped).collect(),
        by_session: by_session.iter().map(grouped).collect(),
        apprentice: ApprenticeStats::default(),
    })
}

fn grouped(g: &GroupedTotals) -> TokenBucket {
    bucket(Some(g.key.clone()), g.label.clone(), &g.totals)
}

fn bucket(key: Option<String>, label: Option<String>, t: &UsageTotals) -> TokenBucket {
    TokenBucket {
        key,
        label,
        calls: t.calls,
        input: t.input_tokens,
        output: t.output_tokens,
        cache_read: t.cache_read_tokens,
        cache_creation: t.cache_creation_tokens,
        cost_usd: micros_to_usd(t.cost_micros),
        unpriced_calls: t.unpriced_calls,
    }
}

async fn blocking<T, F>(f: F) -> Result<T, RpcError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RpcError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| RpcError::internal(format!("stats task failed: {e}")))?
}
