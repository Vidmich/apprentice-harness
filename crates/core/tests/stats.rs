//! Token accounting over recorded mentor calls: aggregation by model, day
//! and session, range bounds, the `stats.*` RPC methods and repricing.

use std::sync::Arc;

use apprentice_api::jsonrpc::codes;
use apprentice_api::methods::{StatsReprice, StatsRepriceParams, StatsTokens, StatsTokensParams};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::types::{TokenBucket, TokenStats, Usage};
use apprentice_client::{ClientError, ClientOptions, DaemonClient};
use apprentice_common::paths::Paths;
use apprentice_core::config::Pricing;
use apprentice_core::stats::{PriceTable, StatsService, price_call};
use apprentice_core::trace::{
    CallId, CallKind, MentorCallEnd, MentorCallStart, NewAgent, NewEvent, NewSession, RunStatus,
    SessionId, TraceStore, kinds,
};
use serde_json::json;
use tempfile::TempDir;
use time::UtcOffset;

const OPUS: &str = "claude-opus-5";
const SONNET: &str = "claude-sonnet-5";
const UNKNOWN: &str = "unknown-model";

struct Fixture {
    _dir: TempDir,
    store: Arc<TraceStore>,
    s1: SessionId,
    s2: SessionId,
    calls: Vec<CallId>,
}

fn table() -> PriceTable {
    [
        (OPUS.to_owned(), Pricing::first_party(5.0, 25.0)),
        (SONNET.to_owned(), Pricing::first_party(2.0, 10.0)),
    ]
    .into()
}

fn usage(i: u64, o: u64, cr: u64, cw: u64) -> Usage {
    Usage {
        input_tokens: i,
        output_tokens: o,
        cache_read_input_tokens: cr,
        cache_creation_input_tokens: cw,
    }
}

fn session(store: &TraceStore, title: &str) -> SessionId {
    store
        .create_session(&NewSession {
            title: Some(title.into()),
            workspace_path: None,
            workspace_id: None,
            config: json!({}),
        })
        .unwrap()
}

/// Records one completed call priced with `table`.
fn call(
    store: &TraceStore,
    sid: &SessionId,
    model: &str,
    started_at: &str,
    usage: Usage,
    table: &PriceTable,
) -> CallId {
    let agent = store
        .start_agent(&NewAgent::main(sid.clone(), "t"))
        .unwrap();
    let step = store.start_step(&agent).unwrap();
    let req = store
        .append(
            NewEvent::new(sid.clone(), kinds::MENTOR_REQUEST)
                .agent(agent.clone())
                .step(step.clone())
                .blob_bytes("{}", "application/json"),
        )
        .unwrap();
    let id = CallId::generate();
    store
        .record_mentor_call(&MentorCallStart {
            id: id.clone(),
            session: sid.clone(),
            agent,
            step,
            request_event: req,
            model: model.into(),
            effort: None,
            request_bytes: Some(2),
            started_at: Some(started_at.into()),
            kind: CallKind::Step,
        })
        .unwrap();
    store
        .complete_mentor_call(&MentorCallEnd {
            usage: Some(usage),
            cost_micros: price_call(model, &usage, table),
            ..MentorCallEnd::new(id.clone(), RunStatus::Ok)
        })
        .unwrap();
    id
}

/// Four calls: two sessions, two days (UTC), two priced models and one
/// unpriced.
///
/// | call | session | model   | started (UTC)    | usage (in/out/cr/cw) | micros |
/// |------|---------|---------|------------------|----------------------|--------|
/// | c1   | alpha   | opus    | 09-10 23:30      | 1000/100/0/0         | 7 500  |
/// | c2   | alpha   | sonnet  | 09-11 08:00      | 2000/200/500/100     | 6 350  |
/// | c3   | beta    | opus    | 09-11 12:00      | 100/10/0/0           | 750    |
/// | c4   | beta    | unknown | 09-11 13:00      | 10/10/0/0            | NULL   |
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(TraceStore::open(&Paths::from_home(dir.path())).unwrap());
    let t = table();
    let s1 = session(&store, "alpha");
    let s2 = session(&store, "beta");
    let calls = vec![
        call(
            &store,
            &s1,
            OPUS,
            "2026-09-10T23:30:00.000Z",
            usage(1000, 100, 0, 0),
            &t,
        ),
        call(
            &store,
            &s1,
            SONNET,
            "2026-09-11T08:00:00.000Z",
            usage(2000, 200, 500, 100),
            &t,
        ),
        call(
            &store,
            &s2,
            OPUS,
            "2026-09-11T12:00:00.000Z",
            usage(100, 10, 0, 0),
            &t,
        ),
        call(
            &store,
            &s2,
            UNKNOWN,
            "2026-09-11T13:00:00.000Z",
            usage(10, 10, 0, 0),
            &t,
        ),
    ];
    Fixture {
        _dir: dir,
        store,
        s1,
        s2,
        calls,
    }
}

fn service(f: &Fixture, offset: UtcOffset) -> StatsService {
    StatsService::with_pricing(Arc::clone(&f.store), table()).with_offset(offset)
}

fn assert_bucket(b: &TokenBucket, calls: u64, input: u64, output: u64, usd: f64, unpriced: u64) {
    assert_eq!(b.calls, calls, "{b:?}");
    assert_eq!(b.input, input, "{b:?}");
    assert_eq!(b.output, output, "{b:?}");
    assert!((b.cost_usd - usd).abs() < 1e-9, "{b:?}");
    assert_eq!(b.unpriced_calls, unpriced, "{b:?}");
}

#[test]
fn totals_by_model_by_day_and_by_session_match_hand_computed_sums() {
    let f = fixture();
    let svc = service(&f, UtcOffset::UTC);
    let s = svc.tokens(&StatsTokensParams::default()).unwrap();

    assert_eq!(s.tz, "+00:00");
    assert_eq!(s.range.since, None);
    assert_bucket(&s.totals, 4, 3110, 320, 0.0146, 1);
    assert_eq!(s.totals.cache_read, 500);
    assert_eq!(s.totals.cache_creation, 100);

    let keys: Vec<_> = s.by_model.iter().map(|b| b.key.as_deref()).collect();
    assert_eq!(keys, vec![Some(OPUS), Some(SONNET), Some(UNKNOWN)]);
    assert_bucket(&s.by_model[0], 2, 1100, 110, 0.00825, 0);
    assert_bucket(&s.by_model[1], 1, 2000, 200, 0.00635, 0);
    assert_bucket(&s.by_model[2], 1, 10, 10, 0.0, 1);

    let days: Vec<_> = s.by_day.iter().map(|b| b.key.as_deref()).collect();
    assert_eq!(days, vec![Some("2026-09-10"), Some("2026-09-11")]);
    assert_bucket(&s.by_day[0], 1, 1000, 100, 0.0075, 0);
    assert_bucket(&s.by_day[1], 3, 2110, 220, 0.0071, 1);

    // Sessions: highest cost first, titles as labels.
    assert_eq!(s.by_session.len(), 2);
    assert_eq!(s.by_session[0].key.as_deref(), Some(f.s1.as_str()));
    assert_eq!(s.by_session[0].label.as_deref(), Some("alpha"));
    assert_bucket(&s.by_session[0], 2, 3000, 300, 0.01385, 0);
    assert_eq!(s.by_session[1].label.as_deref(), Some("beta"));
    assert_bucket(&s.by_session[1], 2, 110, 20, 0.00075, 1);

    assert_eq!(s.apprentice.invocations, 0);
}

#[test]
fn days_follow_the_local_offset() {
    let f = fixture();
    // At +02:00 the 23:30Z call belongs to the 11th, like the others.
    let svc = service(&f, UtcOffset::from_hms(2, 0, 0).unwrap());
    let s = svc.tokens(&StatsTokensParams::default()).unwrap();
    assert_eq!(s.tz, "+02:00");
    assert_eq!(s.by_day.len(), 1);
    assert_eq!(s.by_day[0].key.as_deref(), Some("2026-09-11"));
    assert_bucket(&s.by_day[0], 4, 3110, 320, 0.0146, 1);

    // At -05:00 the 23:30Z call is still the 10th; nothing else moves.
    let svc = service(&f, UtcOffset::from_hms(-5, 0, 0).unwrap());
    let s = svc.tokens(&StatsTokensParams::default()).unwrap();
    assert_eq!(s.tz, "-05:00");
    assert_eq!(s.by_day.len(), 2);
}

#[test]
fn session_filter_returns_only_that_session() {
    let f = fixture();
    let svc = service(&f, UtcOffset::UTC);
    let s = svc
        .tokens(&StatsTokensParams {
            session_id: Some(f.s2.as_str().to_owned()),
            ..Default::default()
        })
        .unwrap();
    assert_bucket(&s.totals, 2, 110, 20, 0.00075, 1);
    assert!(
        s.by_session.is_empty(),
        "by_session is omitted for one session"
    );
    assert_eq!(s.by_model.len(), 2);
    assert_eq!(s.by_day.len(), 1);
}

#[test]
fn range_bounds_accept_dates_timestamps_and_ages() {
    let f = fixture();
    let svc = service(&f, UtcOffset::UTC);
    let tokens = |since: Option<&str>, until: Option<&str>| {
        svc.tokens(&StatsTokensParams {
            since: since.map(str::to_owned),
            until: until.map(str::to_owned),
            session_id: None,
        })
    };

    let s = tokens(Some("2026-09-11"), None).unwrap();
    assert_eq!(s.range.since.as_deref(), Some("2026-09-11T00:00:00.000Z"));
    assert_eq!(s.totals.calls, 3);

    // `until` on a date includes that whole day.
    let s = tokens(None, Some("2026-09-10")).unwrap();
    assert_eq!(s.range.until.as_deref(), Some("2026-09-11T00:00:00.000Z"));
    assert_eq!(s.totals.calls, 1);

    let s = tokens(Some("2026-09-11T12:00:00Z"), Some("2026-09-11T13:00:00Z")).unwrap();
    assert_eq!(s.totals.calls, 1);
    assert_eq!(s.by_model[0].key.as_deref(), Some(OPUS));

    // The fixture is older than an hour on any real clock.
    let s = tokens(Some("1h"), None).unwrap();
    assert_eq!(s.totals.calls, 0);
    assert!(s.by_model.is_empty() && s.by_day.is_empty() && s.by_session.is_empty());

    let err = tokens(Some("yesterday"), None).unwrap_err();
    assert_eq!(err.code, codes::INVALID_PARAMS);
    assert!(err.message.contains("yesterday"), "{err:?}");
}

#[test]
fn reprice_updates_only_selected_rows_and_is_idempotent() {
    let f = fixture();
    let dearer: PriceTable = [
        (OPUS.to_owned(), Pricing::first_party(10.0, 50.0)),
        (SONNET.to_owned(), Pricing::first_party(2.0, 10.0)),
    ]
    .into();
    let svc = StatsService::with_pricing(Arc::clone(&f.store), dearer.clone())
        .with_offset(UtcOffset::UTC);

    // Only the sonnet call is selected: its price is unchanged, so nothing
    // is written even though the opus price moved.
    let r = svc
        .reprice(&StatsRepriceParams {
            model: Some(SONNET.into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!((r.examined, r.changed, r.unpriced), (1, 0, 0));
    assert_eq!(
        f.store.get_mentor_call(&f.calls[0]).unwrap().cost_micros,
        Some(7_500)
    );

    // Whole store: the two opus calls change, the unpriced one stays NULL.
    let r = svc.reprice(&StatsRepriceParams::default()).unwrap();
    assert_eq!((r.examined, r.changed, r.unpriced), (4, 2, 1));
    assert_eq!(
        f.store.get_mentor_call(&f.calls[0]).unwrap().cost_micros,
        Some(15_000)
    );
    assert_eq!(
        f.store.get_mentor_call(&f.calls[2]).unwrap().cost_micros,
        Some(1_500)
    );
    assert_eq!(
        f.store.get_mentor_call(&f.calls[1]).unwrap().cost_micros,
        Some(6_350)
    );
    assert_eq!(
        f.store.get_mentor_call(&f.calls[3]).unwrap().cost_micros,
        None
    );

    // Second run: nothing to do.
    let r = svc.reprice(&StatsRepriceParams::default()).unwrap();
    assert_eq!((r.examined, r.changed, r.unpriced), (4, 0, 1));

    // A range selects by call start.
    let r = svc
        .reprice(&StatsRepriceParams {
            since: Some("2026-09-11".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.examined, 3);

    // Adding the missing price fixes the unpriced call on the next run.
    let mut complete = dearer;
    complete.insert(UNKNOWN.to_owned(), Pricing::first_party(1.0, 1.0));
    let svc = StatsService::with_pricing(Arc::clone(&f.store), complete);
    let r = svc.reprice(&StatsRepriceParams::default()).unwrap();
    assert_eq!((r.examined, r.changed, r.unpriced), (4, 1, 0));
    assert_eq!(
        f.store.get_mentor_call(&f.calls[3]).unwrap().cost_micros,
        Some(20)
    );
    let s = svc.tokens(&StatsTokensParams::default()).unwrap();
    assert_eq!(s.totals.unpriced_calls, 0);
}

#[tokio::test]
async fn stats_methods_over_a_router_round_trip_through_serde() {
    let f = fixture();
    let svc = Arc::new(service(&f, UtcOffset::UTC));
    let expected = svc
        .tokens(&StatsTokensParams {
            session_id: Some(f.s1.as_str().to_owned()),
            ..Default::default()
        })
        .unwrap();

    let mut router = Router::new(RouterConfig {
        daemon_version: "0".into(),
        pid: 1,
        token: None,
    });
    Arc::clone(&svc).register(&mut router);
    assert_eq!(router.methods(), vec!["stats.reprice", "stats.tokens"]);

    let (server_side, client_side) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_side);
    let router = Arc::new(router);
    tokio::spawn(async move {
        let _ = router.serve(sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let client = DaemonClient::from_streams(cr, cw, ClientOptions::default());
    client.hello("test", "0", None).await.unwrap();

    let got: TokenStats = client
        .call::<StatsTokens>(StatsTokensParams {
            session_id: Some(f.s1.as_str().to_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(got, expected);
    assert_bucket(&got.totals, 2, 3000, 300, 0.01385, 0);

    // What `harness stats tokens --json` prints parses back unchanged.
    let text = serde_json::to_string_pretty(&got).unwrap();
    let back: TokenStats = serde_json::from_str(&text).unwrap();
    assert_eq!(back, got);

    let r = client
        .call::<StatsReprice>(StatsRepriceParams::default())
        .await
        .unwrap();
    assert_eq!((r.examined, r.changed, r.unpriced), (4, 0, 1));

    let err = client
        .call::<StatsTokens>(StatsTokensParams {
            since: Some("soon".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    match err {
        ClientError::Rpc(e) => assert_eq!(e.code, codes::INVALID_PARAMS),
        other => panic!("unexpected {other:?}"),
    }
}
