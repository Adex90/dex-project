//! The DarkPool curve planner: a port of `bots/inventory-cpp/include/strawberry/curve.hpp`, operation for operation, so
//! that it returns the C++ planner's numbers bit for bit (the golden tests compare them).
//!
//! The planner turns the Avellaneda–Stoikov bid and ask into a `flashRebalance` of the DarkPool toward the zk oracle
//! price: how much the pool sends out, how much the executor returns, and the Treasury allowance. Everything here is an
//! off-chain estimate in token units (f64); the chain re-checks every rule exactly, so the amounts are rounded in the
//! pool's favour when they become wei (`tx::Rebalance`).

/// Mirrors DarkPool: (R0 + v0)(R1 + v1) = K, in tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PoolState {
    pub r0: f64,
    pub r1: f64,
    pub v0: f64,
    pub v1: f64,
}

impl PoolState {
    pub fn x(&self) -> f64 {
        self.r0 + self.v0
    }
    pub fn y(&self) -> f64 {
        self.r1 + self.v1
    }
    pub fn k(&self) -> f64 {
        self.x() * self.y()
    }
    /// token1 per token0.
    pub fn spot(&self) -> f64 {
        self.y() / self.x()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlannerParams {
    /// DarkPool.EXTRACTION_BPS: the pool pulls exactly this share of the harvest for the Treasury.
    pub extraction_bps: f64,
    /// Headroom on the Treasury allowance (a cap) in case the oracle ticks before inclusion.
    pub treasury_margin_bps: f64,
    /// Expected gas, in token1.
    pub gas_cost_quote: f64,
    /// Minimum expected profit worth the risk, in token1.
    pub min_net: f64,
    /// Executor inventory available to deliver, in tokens.
    pub max_inv0: f64,
    pub max_inv1: f64,
}

impl Default for PlannerParams {
    fn default() -> Self {
        Self {
            extraction_bps: 8000.0,
            treasury_margin_bps: 5.0,
            gas_cost_quote: 0.0,
            min_net: 0.0,
            max_inv0: 0.0,
            max_inv1: 0.0,
        }
    }
}

/// A concrete rebalance: what the pool sends out, what the executor returns, and the Treasury allowance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plan {
    /// True: the pool is above the oracle, the executor sells token0 into it.
    pub sells_token0: bool,
    pub amount0_out: f64,
    pub amount1_out: f64,
    pub pay0: f64,
    pub pay1: f64,
    /// Allowance for the pool's pull (a cap); the pool takes exactly harvest × extraction.
    pub treasury_pay: f64,
    /// Value the pool loses at the oracle price, the gross gain (token1).
    pub harvest: f64,
    /// Gain measured against the A-S bid or ask.
    pub edge: f64,
    /// edge − Treasury cut − gas.
    pub net: f64,
    pub target_price: f64,
}

// C++ std::max, std::min and std::clamp, exactly: f64::max and f64::min treat NaN and ±0 differently.
fn std_max(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}
fn std_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}
fn std_clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if hi < v {
        hi
    } else {
        v
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Eval {
    in0: f64,
    out0: f64,
    in1: f64,
    out1: f64,
    harvest: f64,
    cut: f64,
    cap: f64,
    edge: f64,
    net: f64,
    feasible: bool,
}

#[allow(clippy::too_many_arguments)]
fn eval_at(
    s: &PoolState,
    pt: f64,
    sell: bool,
    oracle_p: f64,
    bid: f64,
    ask: f64,
    pp: &PlannerParams,
) -> Eval {
    let k = s.k();
    let (xt, yt) = ((k / pt).sqrt(), (k * pt).sqrt());
    let mut e = Eval::default();
    if sell {
        // Pool price too high: we give token0 and take token1.
        e.in0 = xt - s.x();
        e.out1 = s.y() - yt;
        e.harvest = e.out1 - e.in0 * oracle_p;
        e.edge = e.out1 - e.in0 * ask;
        e.feasible = e.in0 > 0.0 && e.out1 > 0.0 && e.out1 < s.r1 && e.in0 <= pp.max_inv0;
    } else {
        // Pool price too low: we give token1 and take token0.
        e.in1 = yt - s.y();
        e.out0 = s.x() - xt;
        e.harvest = e.out0 * oracle_p - e.in1;
        e.edge = e.out0 * bid - e.in1;
        e.feasible = e.in1 > 0.0 && e.out0 > 0.0 && e.out0 < s.r0;
    }
    e.cut = std_max(0.0, e.harvest) * (pp.extraction_bps / 1e4);
    e.cap = e.cut * (1.0 + pp.treasury_margin_bps / 1e4);
    if !sell {
        e.feasible = e.feasible && (e.in1 + e.cap <= pp.max_inv1);
    }
    e.net = e.edge - e.cut - pp.gas_cost_quote;
    e
}

/// Chooses the target price pt (how far to push the pool) that maximises the net profit, in closed form:
///
/// ```text
/// sell (spot > P): net(x_t) = (1−φ)(y − K/x_t) − (x_t − x)(ask − φP) − gas  ⇒  p* = (ask − φP)/(1 − φ)
/// buy  (spot < P): net(y_t) = (x − K/y_t)(bid − φP) − (y_t − y)(1 − φ) − gas  ⇒  p* = (bid − φP)/(1 − φ)
/// ```
///
/// with φ the extraction share. Both objectives are concave, so the optimum over the feasible interval is p* clamped to
/// it, and that interval is closed-form too (inventory, real reserves, and always toward the oracle, never past it:
/// the contract requires |spot′ − P| < |spot − P|). Returns None when nothing is feasible or the net is not above
/// `min_net`.
// `!(a > b)` is the C++'s test, kept as is: unlike `a <= b`, it is also true when either side is NaN, so a NaN input
// plans nothing.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub(crate) fn plan(
    s: &PoolState,
    oracle_p: f64,
    bid: f64,
    ask: f64,
    pp: &PlannerParams,
) -> Option<Plan> {
    let spot = s.spot();
    if !(oracle_p > 0.0) || !(spot > 0.0) || (spot - oracle_p).abs() / oracle_p < 1e-9 {
        return None;
    }
    let sell = spot > oracle_p;
    let phi = pp.extraction_bps / 1e4;
    let phi_cap = phi * (1.0 + pp.treasury_margin_bps / 1e4);
    let (k, eps) = (s.k(), 1e-9 * oracle_p);
    let mut lo = if sell { oracle_p + eps } else { spot + eps };
    let mut hi = if sell { spot - eps } else { oracle_p - eps };
    if sell {
        let xmax = s.x() + pp.max_inv0;
        lo = std_max(lo, k / (xmax * xmax));
        let ymin = s.y() - s.r1;
        if ymin > 0.0 {
            lo = std_max(lo, (ymin * ymin / k) * (1.0 + 1e-12));
        }
    } else {
        let xmin = s.x() - s.r0;
        if xmin > 0.0 {
            hi = std_min(hi, (k / (xmin * xmin)) * (1.0 - 1e-12));
        }
        let sk = k.sqrt();
        let a = (1.0 - phi_cap) * sk;
        let b = -((1.0 - phi_cap) * s.y() - phi_cap * oracle_p * s.x() + pp.max_inv1);
        let c = -phi_cap * oracle_p * sk;
        let u = (-b + (b * b - 4.0 * a * c).sqrt()) / (2.0 * a);
        hi = std_min(hi, u * u * (1.0 - 1e-12));
    }
    if !(lo < hi) {
        return None;
    }
    let pt = std_clamp(
        ((if sell { ask } else { bid }) - phi * oracle_p) / (1.0 - phi),
        lo,
        hi,
    );
    let e = eval_at(s, pt, sell, oracle_p, bid, ask, pp);
    if !e.feasible || e.net <= pp.min_net {
        return None;
    }
    Some(Plan {
        sells_token0: sell,
        amount0_out: e.out0,
        amount1_out: e.out1,
        pay0: e.in0,
        pay1: e.in1,
        treasury_pay: e.cap,
        harvest: e.harvest,
        edge: e.edge,
        net: e.net,
        target_price: pt,
    })
}

/// The three DarkPool.flashRebalance acceptance rules (I1, I3a, I3b), re-implemented as in the C++ engine. `cut` is
/// what the pool pulls (80 % of the harvest at P); `treasury_ok` means the allowance covers it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RuleCheck {
    pub k_ok: bool,
    pub deviation_ok: bool,
    pub treasury_ok: bool,
    pub cut: f64,
}

impl RuleCheck {
    #[cfg(test)]
    pub fn all(&self) -> bool {
        self.k_ok && self.deviation_ok && self.treasury_ok
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn chain_rules(
    s: &PoolState,
    oracle_p: f64,
    out0: f64,
    out1: f64,
    in0: f64,
    in1: f64,
    treasury_paid: f64,
) -> RuleCheck {
    let (n0, n1) = (s.r0 - out0 + in0, s.r1 - out1 + in1);
    let (x, y) = (n0 + s.v0, n1 + s.v1);
    // Tolerance for double rounding; the chain is exact.
    let k_ok = x * y >= s.k() * (1.0 - 1e-12);
    let dev_before = (s.spot() - oracle_p).abs() / oracle_p;
    let dev_after = (y / x - oracle_p).abs() / oracle_p;
    let value_before = s.x() * oracle_p + s.y();
    let value_after = x * oracle_p + y;
    let cut = std_max(0.0, value_before - value_after) * 0.8;
    RuleCheck {
        k_ok,
        deviation_ok: dev_after < dev_before,
        treasury_ok: treasury_paid >= cut * (1.0 - 1e-12),
        cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::ulps;
    use crate::planner_golden::PLANS;

    fn params(gas: f64, min_net: f64, inv0: f64, inv1: f64) -> PlannerParams {
        PlannerParams {
            gas_cost_quote: gas,
            min_net,
            max_inv0: inv0,
            max_inv1: inv1,
            ..PlannerParams::default()
        }
    }

    #[test]
    fn the_port_matches_the_cpp_planner_bit_for_bit() {
        let (mut planned, mut worst) = (0, 0);
        for row in PLANS {
            let [
                r0,
                r1,
                v0,
                v1,
                p,
                bid,
                ask,
                gas,
                min_net,
                inv0,
                inv1,
                ok,
                sells,
                a0,
                a1,
                p0,
                p1,
                tr,
                harvest,
                edge,
                net,
                pt,
                k_ok,
                dev_ok,
                tr_ok,
                cut,
            ] = *row;
            let s = PoolState { r0, r1, v0, v1 };
            let got = plan(&s, p, bid, ask, &params(gas, min_net, inv0, inv1));
            assert_eq!(got.is_some(), ok == 1.0, "{row:?}");
            let (b0, b1, q0, q1, t) = match got {
                Some(g) => {
                    planned += 1;
                    assert_eq!(g.sells_token0, sells == 1.0);
                    for (name, x, want) in [
                        ("amount0Out", g.amount0_out, a0),
                        ("amount1Out", g.amount1_out, a1),
                        ("pay0", g.pay0, p0),
                        ("pay1", g.pay1, p1),
                        ("treasuryPay", g.treasury_pay, tr),
                        ("harvest", g.harvest, harvest),
                        ("edge", g.edge, edge),
                        ("net", g.net, net),
                        ("targetPrice", g.target_price, pt),
                    ] {
                        let d = ulps(x, want);
                        assert!(
                            d == 0,
                            "{name}: {x:e} vs C++ {want:e} ({d} ulps) in {row:?}"
                        );
                        worst = worst.max(d);
                    }
                    (g.amount0_out, g.amount1_out, g.pay0, g.pay1, g.treasury_pay)
                }
                None => (0.0, 0.0, 0.0, 0.0, 0.0),
            };
            let r = chain_rules(&s, p, b0, b1, q0, q1, t);
            assert_eq!(
                (r.k_ok, r.deviation_ok, r.treasury_ok),
                (k_ok == 1.0, dev_ok == 1.0, tr_ok == 1.0),
                "{row:?}"
            );
            assert_eq!(ulps(r.cut, cut), 0, "cut {} vs C++ {cut}", r.cut);
        }
        assert!(planned > 150, "the golden set must exercise the planner");
        println!(
            "{} C++ cases, {planned} plans, worst difference {worst} ulps",
            PLANS.len()
        );
    }

    /// The C++ self-test's property: every plan satisfies the contract's rules, never pushes past the oracle, and
    /// stays within the inventory.
    #[test]
    fn every_plan_satisfies_the_contract_rules() {
        let mut rng = crate::chain::SplitMix64(42);
        let mut u = || rng.uniform();
        let mut plans = 0;
        for _ in 0..40_000 {
            let s = PoolState {
                r0: 50.0 + u() * 5000.0,
                r1: 1e5 + u() * 1e7,
                v0: 10.0 + u() * 1000.0,
                v1: 1e4 + u() * 1e6,
            };
            let p = s.spot() * (1.0 + (u() - 0.5) * 0.04);
            let (half, skew) = (u() * 0.002 * p, (u() - 0.5) * 0.002 * p);
            let pp = params(u() * 2.0, 0.0, u() * 200.0, u() * 4e5);
            let Some(g) = plan(&s, p, p + skew - half, p + skew + half, &pp) else {
                continue;
            };
            plans += 1;
            assert!(g.net > 0.0);
            assert!(
                g.pay0 <= pp.max_inv0 * (1.0 + 1e-12)
                    && g.pay1 + g.treasury_pay <= pp.max_inv1 * (1.0 + 1e-12) + g.amount1_out
            );
            assert!(
                chain_rules(
                    &s,
                    p,
                    g.amount0_out,
                    g.amount1_out,
                    g.pay0,
                    g.pay1,
                    g.treasury_pay
                )
                .all()
            );
            assert_eq!(g.sells_token0, s.spot() > p);
            assert!(if g.sells_token0 {
                g.target_price >= p * (1.0 - 1e-9)
            } else {
                g.target_price <= p * (1.0 + 1e-9)
            });
        }
        assert!(
            plans > 1000,
            "the generator must exercise the planner ({plans})"
        );
    }

    #[test]
    fn the_closed_form_is_optimal_against_a_grid() {
        // The C++ self-test's case: pool at 2000, oracle at 1980.
        let s = PoolState {
            r0: 1000.0,
            r1: 2e6,
            v0: 1000.0,
            v1: 2e6,
        };
        let (p, bid, ask) = (1980.0, 1979.0, 1981.0);
        let pp = params(1.0, 0.0, 1e9, 1e12);
        let g = plan(&s, p, bid, ask, &pp).unwrap();
        assert!(g.sells_token0);
        let best = (1..10_000)
            .map(|i| p + (s.spot() - p) * f64::from(i) / 10_000.0)
            .map(|pt| eval_at(&s, pt, true, p, bid, ask, &pp))
            .filter(|e| e.feasible)
            .map(|e| e.net)
            .fold(f64::MIN, f64::max);
        assert!(
            g.net >= best - 1e-6 * best.abs() - 1e-6,
            "closed form {} < grid {best}",
            g.net
        );
    }

    #[test]
    fn nothing_to_do_on_the_oracle_or_without_inventory() {
        let s = PoolState {
            r0: 1000.0,
            r1: 2e6,
            v0: 1000.0,
            v1: 2e6,
        };
        assert!(plan(&s, 2000.0, 1999.0, 2001.0, &params(0.0, 0.0, 1e9, 1e12)).is_none());
        assert!(plan(&s, 1980.0, 1979.0, 1981.0, &params(0.0, 0.0, 0.0, 0.0)).is_none());
        assert!(plan(&s, 1980.0, 1979.0, 1981.0, &params(0.0, 1e9, 1e9, 1e12)).is_none());
        assert!(plan(&s, f64::NAN, 1979.0, 1981.0, &params(0.0, 0.0, 1e9, 1e12)).is_none());
    }
}
