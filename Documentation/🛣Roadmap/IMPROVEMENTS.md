# 🛣 Improvements — PLUGIN.PTP (from the 2026-10-30 architecture audit)

What this repo should change to meet the standard, most valuable first. Source: [🔍Audits/ARCHITECTURE-AUDIT.md](../%F0%9F%94%8DAudits/ARCHITECTURE-AUDIT.md).
← [Roadmap](INDEX.md)

| # | Improvement | Category | Effort (S/M/L) | Evidence |
|---|---|---|---|---|
| 1 | Needs owner: tag this crate's releases (and publish the next SDK tag for standalone builds). | Change management | S | `SRC/Cargo.toml` `version = "0.1.0"` |
| 2 | Needs owner: push LIB.MAC_LOOKUP and publish tag `maclookup-v0.2.0`; then repin `apkaudio-maclookup` from `rev = "7ba39dd…"` to that tag (standalone builds then get `Mac` and the multi-writer cache; workspace builds already do via [patch]). | Change management | S | `SRC/Cargo.toml` `apkaudio-maclookup` (handoff from W03-libs) |

Everything else from the audit is delivered — see [🌍Publishing/DELIVERY.md](../%F0%9F%8C%8DPublishing/DELIVERY.md#delivered-from-the-2026-10-30-audit).
