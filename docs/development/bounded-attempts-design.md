# Bounded adaptive scenario attempts

The knob-dialog Pan demonstration and channel-two LEVEL check select their next input from guest observations. Their bounded attempts need visible input, capture, observation and assertion steps while retaining the original retry and artifact behavior.

## Model

The scenario declares a flat sequence of typed, bounded attempt blocks. The supported block variants describe the actual Pan sweep, Pan reset and channel-two LEVEL fallback; each validates its exact ordered child primitives and declares its numeric limits, direction, target and fallback deltas in TOML. Conditions consume typed, previously produced observations. Validation rejects missing producers, invalid scope, wrong child order, invalid limits and unconsumed evidence. There are no arbitrary expressions, unrestricted nested blocks or generic top-level stop operations.

The six-check/seventh-capture demo behavior is a typed capture child of the Pan attempt, with its own guest duration, brightness threshold and check count supplied in scenario data. It reports each capture and brightness check with the enclosing Pan attempt and frame index; it is not an arbitrary nested attempt language. Ordered channel-two deltas are data, and the reset block explicitly continues after exhaustion.

One canonical executor runs the parsed plan in both ordinary scenarios and the post-update lifecycle, whose plan is bound to the staged executable rather than loaded from a live checkout. The executor invokes shared typed device operations and owns one preflight debugger and shared deadline; it never starts a second guest or competing PanelFeed.

Each block, attempt and child reports start, outcome and wall timing with its parent step and attempt number. The watcher renders the parent step and the current child without fabricating a changing total step count; reports retain completed child timings and failures identify their block, attempt and child. Skipped children do not report execution. The progress protocol version changes so a mismatched watcher refuses the stream.

## Required semantics

- Pan starts with a 100-guest-ms capture, checking at most six frames for 100 lit pixels and capturing a seventh before failing. Preserve artifact names and write each capture immediately.
- Assert the saved text resource is exactly `CENTER`, then save raw channel-one Pan. For left and right, sample raw Pan then text in that order; each succeeds at displayed direction >=48%. Otherwise turn towards raw target 32 or 95, using the existing physical-to-encoder polarity, and capture the bounded demo frame. Each direction permits 100 attempts; exhaustion fails. Capture each endpoint for 500 guest ms.
- Return Pan toward the saved starting raw using up to 100 turns and 50-guest-ms captures. Its original exhaustion continues without adding a new assertion.
- Send target 35 tap for 50 ms without acknowledgement, then wait for selector 9 with 5-ms polling and 10,000 guest-ms / 180-second wall limits.
- Save channel-one LEVEL, perform the original initial channel-two read even if it already differs, and try physical channel-two deltas -1, +1, -2, +2 in order. After each no-ack turn capture 250 guest ms, overwrite the original artifact, then sample channel two. Stop when it differs from channel one. Finally read displayed text once and separately assert both difference and rounded LEVEL text.
- Preserve the preflight debugger, the original 180,000 guest-ms / 600-second wall budget shared across the diagnostic, independent per-capture limits, input routing, frozen fixture identities and all cleanup behavior. The six ordinary consumers and patched-update lifecycle must execute the same plan.

## Validation

Unit-test deterministic plan parsing, limits, expansion/order, child progress and failure attribution, skipped attempts, endpoint polarity, each exhaustion policy, 6/7 capture edge, guest wrap and deadline propagation, snapshot text read ordering, and early cleanup. Compare all generated schema/catalog identities. After independent review run focused L6max firmware, SD and patched-update cases with four workers and pinned executable identities.
