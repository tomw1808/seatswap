//! The one estimator of a factor and its range (spec R2 of `specs/plan-1.3/PLAN.md`):
//! the percent of one limit per API dollar of work, learned from pairs of readings,
//! with an assumed factor as prior. Every factor in auto-burn and the statistics comes
//! from here.

use super::LEARN_MIN_USD;

/// The z value of the two-sided 80 % range of a normal distribution.
const Z_80: f64 = 1.2816;

/// One pair of readings of one limit: the percent it gained and the API dollars
/// of the account's work in between.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pair {
    pub gain: f64,
    pub usd: f64,
}

/// A factor: the percent of one limit one API dollar of work costs (spec R1 of
/// 1.3). `low`/`high` is its 80 % range, `basis_usd` the dollars of the
/// account's own pairs it rests on, and `measured` says that those reach
/// [`LEARN_MIN_USD`]; a factor that is not measured leans on its prior.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Factor {
    pub value: f64,
    pub low: f64,
    pub high: f64,
    pub basis_usd: f64,
    pub measured: bool,
}

/// The factor of `pairs` with an optional prior (spec R2 of 1.3):
///
/// - value: `(Σ gain + prior × w) / (Σ usd + w)` with `w` = [`LEARN_MIN_USD`]
///   while a prior exists, `Σ gain / Σ usd` without one;
/// - range: `se = sqrt(Σ (gain_i − value · usd_i)² + (prior · w − value · w)²)
///   / (Σ usd + w)`, `low = max(0, value − 1.2816 · se)`, `high = value +
///   1.2816 · se`. The prior counts as one more pair `(prior × w, w)` with its
///   own residual: it widens the range when it disagrees with the pairs, and
///   without pairs the range is the prior itself;
/// - `basis_usd = Σ usd`, `measured = Σ usd ≥ LEARN_MIN_USD`.
///
/// `None` without pairs and without a prior, when the dollars are 0 and there
/// is no prior, and when the value is not above 0: a factor that cannot be
/// positive is unknown, not a number. Whether a factor that is neither measured
/// nor has a prior may be used is the caller's rule.
pub fn factor(pairs: &[Pair], prior: Option<f64>) -> Option<Factor> {
    let gain: f64 = pairs.iter().map(|pair| pair.gain).sum();
    let usd: f64 = pairs.iter().map(|pair| pair.usd).sum();
    let (prior_gain, weight) = match prior {
        Some(prior) => (prior * LEARN_MIN_USD, LEARN_MIN_USD),
        None => (0.0, 0.0),
    };
    let dollars = usd + weight;
    if dollars <= 0.0 {
        return None;
    }
    let value = (gain + prior_gain) / dollars;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let mut squares: f64 = pairs
        .iter()
        .map(|pair| (pair.gain - value * pair.usd).powi(2))
        .sum();
    if let Some(prior) = prior {
        squares += (prior * weight - value * weight).powi(2);
    }
    let se = squares.sqrt() / dollars;
    Some(Factor {
        value,
        low: (value - Z_80 * se).max(0.0),
        high: value + Z_80 * se,
        basis_usd: usd,
        measured: usd >= LEARN_MIN_USD,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-12;

    fn pairs() -> Vec<Pair> {
        vec![
            Pair {
                gain: 2.0,
                usd: 20.0,
            },
            Pair {
                gain: 3.0,
                usd: 30.0,
            },
            Pair {
                gain: 1.0,
                usd: 12.0,
            },
        ]
    }

    #[test]
    fn the_factor_is_gain_over_dollars_with_an_80_percent_range() {
        let factor = factor(&pairs(), None).expect("three pairs");
        let value = 6.0 / 62.0;
        assert!((factor.value - value).abs() < EPS, "{}", factor.value);
        let squares = (2.0 - value * 20.0_f64).powi(2)
            + (3.0 - value * 30.0_f64).powi(2)
            + (1.0 - value * 12.0_f64).powi(2);
        let se = squares.sqrt() / 62.0;
        assert!(se > 0.0, "the pairs do not lie on one line");
        assert!(
            (factor.low - (value - 1.2816 * se)).abs() < EPS,
            "{factor:?}"
        );
        assert!(
            (factor.high - (value + 1.2816 * se)).abs() < EPS,
            "{factor:?}"
        );
        assert!((factor.basis_usd - 62.0).abs() < EPS);
        assert!(!factor.measured, "62 USD is less than 100");

        // The same pairs ten times over are measured, with a narrower range.
        let many: Vec<Pair> = pairs().into_iter().cycle().take(30).collect();
        let measured = super::factor(&many, None).expect("thirty pairs");
        assert!(measured.measured, "620 USD");
        assert!((measured.value - value).abs() < EPS);
        assert!(measured.high - measured.low < factor.high - factor.low);

        // A range never reaches below 0.
        let wide = super::factor(
            &[
                Pair {
                    gain: 10.0,
                    usd: 1.0,
                },
                Pair {
                    gain: 0.0,
                    usd: 99.0,
                },
            ],
            None,
        )
        .expect("two pairs");
        assert_eq!(wide.low, 0.0, "{wide:?}");
    }

    #[test]
    fn a_prior_weighs_like_100_dollars() {
        // 100 USD of own work at 0.2 % per USD next to a prior of 0.1: half way.
        let own = [Pair {
            gain: 20.0,
            usd: 100.0,
        }];
        let blended = factor(&own, Some(0.1)).expect("a pair and a prior");
        assert!((blended.value - 0.15).abs() < EPS, "{}", blended.value);
        assert!(blended.measured, "100 USD of its own");
        assert!((blended.basis_usd - 100.0).abs() < EPS);

        // Without pairs the prior is the factor, not measured, with no range.
        let assumed = factor(&[], Some(0.15)).expect("a prior");
        assert_eq!(
            assumed,
            Factor {
                value: 0.15,
                low: 0.15,
                high: 0.15,
                basis_usd: 0.0,
                measured: false,
            }
        );

        // 25 USD move a prior by a fifth of the way.
        let few = [Pair {
            gain: 5.0,
            usd: 25.0,
        }];
        let moved = factor(&few, Some(0.1)).expect("a pair and a prior");
        assert!(
            (moved.value - (5.0 + 10.0) / 125.0).abs() < EPS,
            "{}",
            moved.value
        );
        assert!(!moved.measured);
    }

    #[test]
    fn a_prior_that_disagrees_widens_the_range() {
        // The pair (20, 100) and a prior of 0.1: the value is 0.15, and the
        // prior's own residual (10 − 15)² is counted beside the pair's (20 −
        // 15)², so se = sqrt(50) / 200.
        let own = [Pair {
            gain: 20.0,
            usd: 100.0,
        }];
        let factor = factor(&own, Some(0.1)).expect("a pair and a prior");
        let se = 50.0_f64.sqrt() / 200.0;
        assert!((factor.value - 0.15).abs() < EPS, "{}", factor.value);
        assert!(
            (factor.low - (0.15 - 1.2816 * se)).abs() < EPS,
            "{factor:?}"
        );
        assert!(
            (factor.high - (0.15 + 1.2816 * se)).abs() < EPS,
            "{factor:?}"
        );
        assert!(factor.high - factor.low > 0.0, "{factor:?}");
    }

    #[test]
    fn no_pairs_and_no_prior_or_no_positive_value_is_no_factor() {
        assert!(factor(&[], None).is_none());
        let no_work = [Pair {
            gain: 4.0,
            usd: 0.0,
        }];
        assert!(factor(&no_work, None).is_none(), "a rise without work");
        let falling = [Pair {
            gain: -1.0,
            usd: 150.0,
        }];
        assert!(factor(&falling, None).is_none());
        assert!(
            factor(&falling, Some(0.001)).is_none(),
            "(−1 + 0.1) / 250 < 0"
        );
    }
}
