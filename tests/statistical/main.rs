//! Non-parity statistical target (§14A, §9.4).
//!
//! `tests/simulation/test_models.py::test_ticker_ltp` does seed-exact RNG
//! parity against Python `random.gauss(0,1)` + `random.seed(1000)` — the
//! byte-semantics of that RNG aren't reproducible from Rust. PORT-PLAN §14A
//! replaces it with this statistical assertion: over 1000 samples, the mean
//! of `Z` should be close to 0 and the std close to 1, so Ticker's
//! perturbation distribution is the one the spec asks for.
//!
//! Uses `libtest-mimic` to match the rest of the harness style. This
//! target is gated on `#[[test]] required-features = ["statistical-tests"]`
//! in `Cargo.toml` so it only runs when explicitly requested — it's not
//! part of the 237-item parity denominator.

use libtest_mimic::{run, Arguments, Failed, Trial};
use omsrs::simulation::Ticker;
use std::process::ExitCode;

fn wrap(name: &'static str, f: fn()) -> Trial {
    Trial::test(name, move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match res {
            Ok(()) => Ok(()),
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "statistical test panicked".to_string()
                };
                Err(Failed::from(msg))
            }
        }
    })
}

fn test_ticker_ltp_statistical() {
    // Generate 1000 samples via Ticker's RNG path. Upstream equivalent
    // would use `random.seed(1000)`; we seed `SmallRng::seed_from_u64(1000)`
    // to match the spirit (reproducible) without claiming byte parity.
    let ticker = Ticker::with_seed("aapl", 125.0, 1000);
    let n = 1000usize;
    let mut ratios = Vec::with_capacity(n);
    let mut prev = ticker.ltp_snapshot();
    for _ in 0..n {
        let cur = ticker.ltp();
        // Normalise the tick-rounded move back to Z-scale: (cur - prev) /
        // (prev * 0.01). The rounding to 0.05 introduces a small bias so
        // tolerances are generous.
        let z = (cur - prev) / (prev * 0.01);
        ratios.push(z);
        prev = cur;
    }
    let mean: f64 = ratios.iter().sum::<f64>() / (n as f64);
    let var: f64 = ratios.iter().map(|z| (z - mean).powi(2)).sum::<f64>() / (n as f64);
    let std = var.sqrt();
    // Generous bounds — 0.05 tick-rounding introduces per-step bias; over
    // 1000 samples the Normal(0,1) shape should still be visible.
    assert!(
        mean.abs() < 0.2,
        "Ticker Z-mean drifted too far: {mean:.4}"
    );
    assert!(
        std > 0.7 && std < 1.3,
        "Ticker Z-std out of [0.7, 1.3]: {std:.4}"
    );
}

fn main() -> ExitCode {
    let args = Arguments::from_args();
    let trials = vec![wrap("test_ticker_ltp_statistical", test_ticker_ltp_statistical)];
    let conclusion = run(&args, trials);
    if conclusion.has_failed() {
        ExitCode::from(1)
    } else {
        ExitCode::from(0)
    }
}
