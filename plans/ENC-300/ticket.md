# ENC-300 — host-declared tool-call format (vision: Linear ENC-300, ratified design incl. shape names)
Session config declares format: Structured|Dsml|Cdata|BracketArray|XmlToolCall|RawJson (+ LiteRtCall renamed by shape).
Parsers behind internal port; only declared parser runs; sniffing dispatch + structured-wins precedence deleted.
Structured: text never mined. Envelope: gate/leak markers = declared format only; wrong format = loud parse failure.
Argument repair out of scope. Stacked on ENC-299 (#31); format rides typed config (ENC-297 surface + FFI enum).
