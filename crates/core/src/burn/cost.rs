//! What a switch costs (spec §2.13): the target's weekly percent gained between
//! the first and the last sample of the ten minutes after it, minus what the
//! work between those two samples should have cost, in API dollars.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

use crate::api::LimitKind;
use crate::history::{
    same_reset, AccountRef, Event, EventKind, Hosted, Range, Sample, TokenRecord,
};
use crate::money::Prices;

use super::decide::SwitchCost;
use super::rates::{timelines, work_in};
use super::Rates;

/// The minutes after a switch that are measured.
const WINDOW_MIN: i64 = 10;

/// For each `Switch` event at a time in `range`: the target account's week-%
/// gained between the first and the last of its samples in the ten minutes
/// after it, minus `c` × the API dollars of work done between those two samples,
/// divided by `c` (so: the extra dollars of work the switch cost; `c` is the
/// week factor's value). The samples are the
/// target's samples of every host whose time lies in the window and whose
/// weekly limit names the same window ([`same_reset`]) as the first one's; the
/// window's first and last sample must be two different samples. The work is
/// the work of every host while the target was live there ([`work_in`], a
/// record spread evenly over its span), between the two samples; a switch whose
/// window holds a record without a price is left out entirely (documented
/// rule), never measured with that work missing. Switches
/// without two such samples, or whose target has no entry in `rates`, are left
/// out; a negative result counts as 0. The result is the mean over the
/// switches that remain, `None` when none remains. `samples` and `tokens` are
/// sorted by time, as a history read returns them: each switch looks only at
/// its own ten minutes.
pub fn switch_cost(
    events: &[Hosted<Event>],
    samples: &[Hosted<Sample>],
    tokens: &[Hosted<TokenRecord>],
    rates: &BTreeMap<AccountRef, Rates>,
    prices: &Prices,
    range: &Range,
) -> Option<SwitchCost> {
    debug_assert!(
        samples.is_sorted_by_key(|hosted| hosted.record.t),
        "switch_cost needs the samples sorted by time"
    );
    let mut sum = 0.0;
    let mut count = 0u32;
    let timelines = timelines(events);
    for hosted in events {
        let EventKind::Switch { to, .. } = &hosted.record.kind else {
            continue;
        };
        let at = hosted.record.t;
        if at < range.from || at >= range.to {
            continue;
        }
        let Some(rates) = rates.get(to) else {
            continue;
        };
        let c = rates.c.value;
        if c <= 0.0 {
            continue;
        }
        let Some((first, last)) = target_pair(samples, to, at) else {
            continue;
        };
        let (Some(before), Some(after)) = (
            first.record.limit(&LimitKind::WeeklyAll),
            last.record.limit(&LimitKind::WeeklyAll),
        ) else {
            continue;
        };
        let work = work_in(
            &timelines,
            tokens,
            to,
            &[(first.record.t, last.record.t)],
            prices,
        );
        if work.iter().any(|span| span.unpriced) {
            continue;
        }
        let usd: f64 = work.iter().map(|span| span.usd).sum();
        let gain = after.p - before.p;
        let cost = (gain - c * usd) / c;
        sum += cost.max(0.0);
        count += 1;
    }
    if count == 0 {
        None
    } else {
        Some(SwitchCost {
            usd: sum / count as f64,
            samples: count,
        })
    }
}

/// The target's first and last sample in the ten minutes after `at`, both
/// carrying a weekly limit of the first one's window. `None` unless they are two
/// different samples. `samples` are sorted by time, as a history read returns
/// them, so only the ten minutes are looked at.
fn target_pair<'a>(
    samples: &'a [Hosted<Sample>],
    account: &AccountRef,
    at: DateTime<Utc>,
) -> Option<(&'a Hosted<Sample>, &'a Hosted<Sample>)> {
    let end = at + Duration::minutes(WINDOW_MIN);
    let from = samples.partition_point(|hosted| hosted.record.t < at);
    let to = samples.partition_point(|hosted| hosted.record.t <= end);
    let mut found: Vec<&Hosted<Sample>> = samples[from..to.max(from)]
        .iter()
        .filter(|hosted| &hosted.record.account == account)
        .filter(|hosted| hosted.record.limit(&LimitKind::WeeklyAll).is_some())
        .collect();
    found.sort_by(|a, b| {
        a.record
            .t
            .cmp(&b.record.t)
            .then_with(|| a.host.cmp(&b.host))
    });
    let first = *found.first()?;
    let reset = first.record.limit(&LimitKind::WeeklyAll)?.r?;
    found.retain(|hosted| {
        hosted
            .record
            .limit(&LimitKind::WeeklyAll)
            .and_then(|limit| limit.r)
            .is_some_and(|other| same_reset(reset, other))
    });
    let last = *found.last()?;
    if std::ptr::eq(first, last) {
        None
    } else {
        Some((first, last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::history::{By, LimitSample, Source};
    use crate::project::ProjectKey;

    const EPS: f64 = 1e-9;
    const RESET: &str = "2026-11-16T16:00:00Z";

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().expect("test date")
    }

    fn target() -> AccountRef {
        AccountRef {
            acc: "6f1c2a9e-1d4b-4c7f-9a3e-8b5d2e6f1c04".to_string(),
            org: "a92d7b31-5c8e-4f2a-9d6b-1e3c5a7f9b20".to_string(),
        }
    }

    fn other() -> AccountRef {
        AccountRef {
            acc: "3b7d1f04-9c2e-4a61-8d5b-0e7f2a4c9b13".to_string(),
            org: "c41e6b82-7d3a-4f95-b2c8-5a1d9e3f7b46".to_string(),
        }
    }

    fn range() -> Range {
        Range {
            from: utc("2026-11-16T09:00:00Z"),
            to: utc("2026-11-16T13:00:00Z"),
        }
    }

    fn rates(c: f64) -> BTreeMap<AccountRef, Rates> {
        BTreeMap::from([(target(), Rates::given(c, None))])
    }

    /// The models.dev fixture's prices: `claude-opus-5-5` costs 4 USD per
    /// million input tokens.
    fn prices() -> Prices {
        crate::money::tests::priced_prices(utc("2026-01-01T00:00:00Z"))
    }

    fn sample(host: &str, at: &str, account: AccountRef, percent: f64) -> Hosted<Sample> {
        Hosted {
            host: host.to_string(),
            record: Sample {
                t: utc(at),
                account,
                src: Source::Poll,
                l: vec![LimitSample {
                    k: LimitKind::WeeklyAll,
                    m: None,
                    p: percent,
                    r: Some(utc(RESET)),
                    b: false,
                }],
            },
        }
    }

    fn switched(host: &str, at: &str, to: AccountRef) -> Hosted<Event> {
        Hosted {
            host: host.to_string(),
            record: Event {
                t: utc(at),
                kind: EventKind::Switch {
                    from: None,
                    to,
                    by: By::User,
                    reason: None,
                    cwd: None,
                    sessions: None,
                    waited_secs: None,
                },
            },
        }
    }

    /// A `live` event: the account is live on that host from then on.
    fn live(host: &str, at: &str, account: Option<AccountRef>) -> Hosted<Event> {
        Hosted {
            host: host.to_string(),
            record: Event {
                t: utc(at),
                kind: EventKind::Live { account },
            },
        }
    }

    /// An hourly token line of `claude-opus-5-5` worth `usd` API dollars (its
    /// input tokens at 4 USD per million).
    fn token(host: &str, at: &str, usd: f64) -> Hosted<TokenRecord> {
        Hosted {
            host: host.to_string(),
            record: TokenRecord {
                t: utc(at),
                minutes: 60,
                session: "7c1e4b2a-9d3f-4e6a-b8c5-2f0a1d7e9b34".to_string(),
                cwd: "/home/sam/projects/atlas".to_string(),
                project: ProjectKey::Repo("github.com/sam/atlas".to_string()),
                model: "claude-opus-5-5".to_string(),
                input: (usd * 250_000.0).round() as u64,
                out: 0,
                cw: 0,
                cr: 0,
                answers: 1,
                agent_answers: 0,
            },
        }
    }

    #[test]
    fn the_extra_percent_after_a_switch_is_the_cost() {
        // A 12.0 USD hour starting at 10:00 puts 2.0 USD into [10:00, 10:10).
        let events = vec![switched("atlas", "2026-11-16T10:00:00Z", target())];
        let samples = vec![
            sample("vega", "2026-11-16T10:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T10:10:00Z", target(), 41.0),
        ];
        let tokens = vec![token("atlas", "2026-11-16T10:00:00Z", 12.0)];

        let cost = switch_cost(
            &events,
            &samples,
            &tokens,
            &rates(0.32),
            &prices(),
            &range(),
        )
        .expect("one switch with two samples");
        assert!((cost.usd - 1.125).abs() < EPS, "{}", cost.usd);
        assert_eq!(cost.samples, 1);
    }

    #[test]
    fn a_switch_without_samples_is_left_out() {
        let events = vec![switched("atlas", "2026-11-16T10:00:00Z", target())];
        assert!(switch_cost(&events, &[], &[], &rates(0.32), &prices(), &range()).is_none());

        // One sample is not two different samples either.
        let samples = vec![sample("vega", "2026-11-16T10:00:00Z", target(), 40.0)];
        assert!(switch_cost(&events, &samples, &[], &rates(0.32), &prices(), &range()).is_none());
    }

    #[test]
    fn less_than_expected_counts_as_zero() {
        // 0.5 % for 2.0 USD would have cost 0.64 %: (0.5 - 0.64) / 0.32 < 0.
        let events = vec![switched("atlas", "2026-11-16T10:00:00Z", target())];
        let samples = vec![
            sample("vega", "2026-11-16T10:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T10:10:00Z", target(), 40.5),
        ];
        let tokens = vec![token("atlas", "2026-11-16T10:00:00Z", 12.0)];

        let cost = switch_cost(
            &events,
            &samples,
            &tokens,
            &rates(0.32),
            &prices(),
            &range(),
        )
        .expect("one switch with two samples");
        assert!((cost.usd - 0.0).abs() < EPS, "{}", cost.usd);
        assert_eq!(cost.samples, 1);
    }

    #[test]
    fn the_mean_over_three_switches() {
        // c = 1.0: switch costs of (1 - 0), (3 - 1) and (5 - 2) USD of work.
        let events = vec![
            switched("atlas", "2026-11-16T10:00:00Z", target()),
            switched("vega", "2026-11-16T11:00:00Z", target()),
            switched("atlas", "2026-11-16T12:00:00Z", target()),
        ];
        let samples = vec![
            sample("vega", "2026-11-16T10:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T10:10:00Z", target(), 41.0),
            sample("vega", "2026-11-16T11:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T11:10:00Z", target(), 43.0),
            sample("vega", "2026-11-16T12:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T12:10:00Z", target(), 45.0),
        ];
        let tokens = vec![
            token("vega", "2026-11-16T11:00:00Z", 6.0),
            token("atlas", "2026-11-16T12:00:00Z", 12.0),
        ];

        let cost = switch_cost(&events, &samples, &tokens, &rates(1.0), &prices(), &range())
            .expect("three switches with two samples each");
        assert!((cost.usd - 2.0).abs() < EPS, "{}", cost.usd);
        assert_eq!(cost.samples, 3);
    }

    #[test]
    fn a_switch_to_an_account_without_rates_is_left_out() {
        let events = vec![switched("atlas", "2026-11-16T10:00:00Z", target())];
        let samples = vec![
            sample("vega", "2026-11-16T10:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T10:10:00Z", target(), 41.0),
        ];
        let tokens = vec![token("atlas", "2026-11-16T10:00:00Z", 12.0)];
        let rates = BTreeMap::from([(other(), Rates::given(0.32, None))]);

        assert!(switch_cost(&events, &samples, &tokens, &rates, &prices(), &range()).is_none());
    }

    // ---- proof tests of the review ----

    #[test]
    fn the_work_of_every_host_where_the_target_is_live_counts() {
        // The switch happened on atlas, but the target was live on vega all the
        // time. The percent the target gained is the percent of the work of
        // both hosts, so vega's work counts to the cost as well: 2.0 USD are
        // worth 0.64 % and the rest is what the switch cost.
        let events = vec![
            live("vega", "2026-11-16T09:00:00Z", Some(target())),
            switched("atlas", "2026-11-16T10:00:00Z", target()),
        ];
        let samples = vec![
            sample("vega", "2026-11-16T10:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T10:10:00Z", target(), 41.0),
        ];
        let tokens = vec![token("vega", "2026-11-16T10:00:00Z", 12.0)];

        let cost = switch_cost(
            &events,
            &samples,
            &tokens,
            &rates(0.32),
            &prices(),
            &range(),
        )
        .expect("one switch with two samples");
        assert!((cost.usd - 1.125).abs() < EPS, "{}", cost.usd);
        assert_eq!(cost.samples, 1);
    }

    /// A switch whose measured window holds work of a model without a price is
    /// left out of the switch cost entirely, never measured with that work
    /// missing.
    #[test]
    fn a_record_without_a_price_is_left_out_of_the_cost() {
        let events = vec![switched("atlas", "2026-11-16T10:00:00Z", target())];
        let samples = vec![
            sample("vega", "2026-11-16T10:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T10:10:00Z", target(), 41.0),
        ];
        let mut unpriced = token("atlas", "2026-11-16T10:00:00Z", 12.0);
        unpriced.record.model = "claude-new-9".to_string();

        assert!(
            switch_cost(
                &events,
                &samples,
                &[unpriced],
                &rates(0.32),
                &prices(),
                &range(),
            )
            .is_none(),
            "a window of unpriced work gives no sample"
        );
    }

    /// 2 USD of priced work beside one unpriced record still give no sample:
    /// the switch is left out whole, not measured with the unpriced work
    /// missing.
    #[test]
    fn unpriced_work_is_no_switch_cost() {
        let events = vec![switched("atlas", "2026-11-16T10:00:00Z", target())];
        let samples = vec![
            sample("vega", "2026-11-16T10:00:00Z", target(), 40.0),
            sample("vega", "2026-11-16T10:10:00Z", target(), 41.0),
        ];
        let mut unpriced = token("atlas", "2026-11-16T10:00:00Z", 60.0);
        unpriced.record.model = "claude-new-9".to_string();
        let tokens = vec![token("atlas", "2026-11-16T10:00:00Z", 2.0), unpriced];

        assert!(
            switch_cost(
                &events,
                &samples,
                &tokens,
                &rates(0.32),
                &prices(),
                &range(),
            )
            .is_none(),
            "the unpriced record leaves the whole switch out"
        );
    }
}
