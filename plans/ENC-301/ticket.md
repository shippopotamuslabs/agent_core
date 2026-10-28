# ENC-301 — state-ledger precedence (vision: Linear ENC-301)
State precedence among ledger / HistorySnapshot / host Summary / surviving log per fact class
(objective, constraints, decisions, open questions, artifact anchors). Later explicit reversal in log
updates/retires ledger entry deterministically in observe_* path. Rule in code + state_ledger docs.
Acceptance: decision->reversal through compaction test pins next render. Stacked on ENC-300 (#32).
