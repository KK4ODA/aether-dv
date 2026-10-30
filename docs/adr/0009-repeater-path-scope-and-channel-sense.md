# ADR-0009: Repeater operation: per-port path and scope, route guard, channel sense

Status: **Accepted** 2026-10-30 (owner decision) · Proposed: 2026-10-30 · Detail: [`10-repeater-operation.md`](../10-repeater-operation.md)

## Context
Most use will be through D-STAR repeaters, often linked to reflectors. The radio, not the application, builds the RF header. Its R2 field decides whether data stays on the repeater (R2 not the gateway) or goes to whatever reflector the repeater is linked to (R2 = `xxxxxx G`, UR `CQCQCQ`). MMDVMHost and ircDDBGateway source confirms this. With R2 = `G`, any UR other than `CQCQCQ` is a gateway command: link, unlink, echo or call-sign route.

D-RATS and its forks neither read nor limit these fields and detect no voice activity. Aether DV currently does the same: busy means "data bytes arrived", and timing is a simplex placeholder.

The Icom IC-9700 CI-V guide documents read/set of UR/R1/R2 (`1F 01`) and a DV RX status that includes "receiving a digital voice call" (`20 02`). Support on other models is unverified.

## Decision
1. **Scope is a property of a radio port**: direct, this repeater, or the linked reflector. It is chosen in setup, shown at all times, and defaults to *this repeater* for repeater ports. There is no per-message scope.
2. **Route guard before transmitting.**
   - The engine compares the radio's UR/R1/R2 with the port's scope: read via CI-V where documented, otherwise confirmed by the operator once per session.
   - A mismatch **holds** the queue; nothing is sent.
   - UR other than `CQCQCQ` with R2 = `G` is always blocked.
   - Writing the radio's call signs needs `allow_set` and a hardware-tested model profile.
3. **Channel sense replaces data-only busy detection.** The scheduler takes channel events from several sources: data bytes, CI-V RX status, squelch and RX call signs. It yields to voice with a break-in gap. The UI states which sources are live.
4. **Courtesy is enforced in the scheduler.**
   - Per-path presets for hold-off, gap between overs and max over.
   - A rolling air-time budget, with Control and Chat given a reserve.
   - An operator *Pause sending*.
   - ARQ timers pause while others hold the channel.
5. **Reflector scope is opt-in twice** (setup + once per session, naming the linked reflector when known). Files are limited, broadcast auto-replies are off, and Fast Data is off unless overridden.
6. **Radios without CI-V route and status support** get the same safeguards, with operator-confirmed routes and inferred or manual busy control (`10` §4.11). The UI always names the source of each fact.
7. **No DDT2 wire change.** All of this is sender-local (ADR-0003), so D-RATS stations on the same repeater interoperate unchanged.

## Consequences
- Accidental reflector-wide traffic needs two explicit choices plus a matching radio setting, instead of one forgotten radio field.
- Voice users are protected on radios with CI-V status. Elsewhere, the app says that voice is not sensed and offers a manual pause.
- More configuration surface. It is mitigated by presets, a guided setup and collapsed advanced settings.
- Several behaviours depend on hardware facts not yet verified (`10` §7). Preset values are placeholders until then.

## Decision record (2026-10-30)
The owner accepted the recommendations:
- the default scope is *this repeater*
- reflector scope is offered with safeguards
- the radio's call signs are written only with `allow_set` on hardware-tested models
- air-time budgets are 25 % per 10 min on a repeater and 10 % on a reflector
