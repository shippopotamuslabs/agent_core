# ENC-301 Research (scouts)
- Ledger: tag-prefix extraction (objective/constraint/decision/question/file), append-only Vecs, objective set-once, artifact anchors keyed-replace (only update semantics). merge_summary re-observes summary text (duplicates/resurrection possible).
- HistorySnapshot: verbatim ContextStore archive, browsable ref only, replaces nothing; minted up to 2x/pass (whole + range).
- Summary: host LLM CompactSummary message inline; ledger.merge_summary re-parses it. Render carries all three (summary inline, snapshot ref+preview, ledger full+delta).
- No semantic-reversal metadata exists; superseded/rewrite_of is compaction-machinery only. Any reversal rule must use the existing explicit tag protocol (deterministic), not NL inference.
