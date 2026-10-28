# ENC-308 — vision (Sean, 2026-10-28)

Sean's commission, verbatim in substance: "Bundle everything in there that
needs changing. I really just want to do this one final ticket and say,
alright, the refactor of this is done. We're ready to hook up a real
second Greenfield consumer and see if this thing's built the right way —
see if the APIs are understandable, well designed, and easy to integrate
with."

Then, same morning, three rulings that shape this ticket:

1. The typing program is the point: after this ticket, nothing crosses
   the FFI as JSON unless there is a specific good reason it should be
   JSON and not a strongly typed argument. Ratified exceptions, with
   reasons: tool arguments/results (host owns the tool schemas; the
   engine validates against the host-declared typed hint + semantics on
   receipt); schema-hint default values (FfiDefaultValue.value_json —
   the value's shape is the tool's business); the session snapshot blob
   (host is a conduit, not a reader; ENC-305 D7; import fully validates).
   Invariant: every surviving JSON crossing is parsed exactly once, at
   the boundary, with typed errors.
2. The extras bags are OUT (Sean, ~10:10 EDT): agent_core defines the
   fields it accepts and that is all that crosses. Host annotations stay
   host-side, keyed by message ID, reattached by the host to engine
   emissions. Unknown keys at the boundary are a typed error, not a
   silent drop and not a round-trip.
3. Execution: "use the same synthitect workflow we have been using to
   work through the other tickets and complete this final refactor"
   (2026-10-28 11:29 EDT). ENC-298's status updated first (done: Merged).

Scope: Linear issue ENC-308 (14 items, Parts A/B/C) is the contract.
