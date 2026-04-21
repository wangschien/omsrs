# omsrs

**Rust port of [omspy](https://github.com/uberdeveloper/omspy)** — a broker-agnostic OMS for trading.

This is a ground-up Rust implementation of omspy's core (Order lifecycle + Broker trait + paper-simulation engine), with venue extensions for **Polymarket** and **Kalshi** prediction markets.

## Relationship to omspy

- **Core abstraction**: `Broker` trait, `Order` lifecycle, `Position` / `Trade` / `OrderBook` models — ported from omspy's `base.py`, `order.py`, `models.py`.
- **Paper engine**: `omsrs::paper` — ported from `omspy/simulation/`.
- **Scope**: Indian-broker adapters (Zerodha / ICICI / Finvasia / Neo / Noren) are **not ported**. omsrs targets prediction-market venues.
- **License**: MIT, matching omspy upstream.

See `~/poly/docs/omsrs-port-plan-v1.md` for the port plan and phase schedule.

## Status

Early — scaffolding only. Port plan under codex review.

## Upstream reference

- omspy (upstream): https://github.com/uberdeveloper/omspy
- omspy local: `~/refs/omspy/`

## License

MIT. See `LICENSE`.
