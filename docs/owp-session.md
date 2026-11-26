# The Open Watch Party session: what a host must do

`plx_owp::party::Party` is one participant's watch party, sans-IO: it implements SDK.md §3–§15 of
[Open Watch Party](https://github.com/edde746/openwatchparty) and decides; the host owns the sockets, the
list fetch, the player and the resolver, and does what the effects say. This document is the contract
between the two. The crate's layer is in [module-layers.md](module-layers.md).

A host builds it with `Party::checked`, which refuses a `name` or `client` that fails SDK.md §7.1 with
`invalid_message`. `Party::new` skips those checks; a name the send checks refuse then fails `create` and
`join` with `invalid_message`.

## Inputs

Every `PartyInput` carries `at: Moment`: the local monotonic time in ms, and the attached adapter's reads
(position, paused, buffering, duration, first frame shown, seek accuracy), or `None` when no adapter is
attached. The reads ride on every input, so a step or a status always sees the player as it is.

| Input | When the host sends it |
|---|---|
| `Call { call, request }` | An API call (SDK.md §16.2), with an ID the host picks. |
| `Net { conn, event }` | `Opened`, `OpenFailed`, `Text` or `Closed(code)` on a connection the session opened. |
| `Got { get, body }` | The list fetch answered, or failed (`None`). |
| `CommandDone { cmd, threw }` | An adapter command finished or threw. |
| `Resolved { search, answer }` | The resolver answered, or threw (`None`). |
| `Probed { probe, duration_ms }` | A probe returned a duration, or nothing or a throw (`None`). |
| `Drawn { u }` | The answer to `Draw`: a number in [0, 1). |
| `Wake` | `next_wake_ms()` has come. |

## Effects

| Effect | What the host does |
|---|---|
| `Open { conn, url }`, `Send { conn, text }`, `Close { conn, code }` | Open, write to and close a WebSocket. |
| `Get { get, url }`, `AbortGet { get }` | Fetch the public relay list under SDK.md §3.4, and abandon it. |
| `Command { cmd, player, action }` | Call the adapter: play, pause, seek or set the rate. One per adapter at a time. |
| `Resolve`, `CancelResolve`, `Probe`, `CancelProbe` | Run and cancel the resolver and a candidate's probe. |
| `Done { call, outcome }` | Complete call `call`. |
| `Event(..)` | Tell the app (SDK.md §15). |
| `Draw` | Draw a random number and send `Drawn`. |

`state()` is the published state after the last input, and `room_position_ms(now)` the room's position.

## IDs and cancellation

- The host names the calls. Every call gets exactly one `Done`. `close` and `detachPlayer` complete once no
  released adapter has a command running or queued (SDK.md §11.2).
- The session names connections, fetches, commands, searches and probes, from one counter: an ID is never
  reused.
- Every cancellation is an effect: `Close`, `AbortGet`, `CancelResolve` or `CancelProbe`. An input that names
  an ID no longer live is ignored.
- The session closes every connection it gives up, also one still opening. One the host reported closed or
  failed gets no `Close`.
- A connection that already sent `create`, `join` or `resume` outlives a cancel. A `joined` answer gets
  `leave` and a close, and an `error` answer only a close. Without an answer it closes at its 10 s answer
  timeout.

## Time

- Deadlines are absolute local times, kept in the session. `next_wake_ms()` is the earliest.
- A `Wake` fires the earliest deadline due at its `now_ms`, one per `Wake`. Deadlines due at the same time
  fire in the order they were set.
- A host that is late calls `Wake` until `next_wake_ms()` is after now. Each of those calls fires a deadline,
  so a `Wake` that is due always makes progress, and one before any deadline is due changes nothing.
- Other inputs never fire a deadline.
- A `now_ms` earlier than the last input's counts as the last one: the session's time never goes back.
- A deadline keeps its whole delay, also past 2147483647 ms. A host whose timers can't wait that long wakes
  early, and that `Wake` does nothing.
- After `close` the host keeps sending `Wake` until `next_wake_ms()` is `None`. A connection that outlives the
  cancel closes on one of those, and a list fetch in flight is abandoned.

## Randomness

The reconnect backoff's `u` (SDK.md §14) is the only random number. The session asks for it with `Draw` and
waits for `Drawn`.

## How it is checked

- `party.json`, the 145 traces of OWP's conformance vectors, runs in `party_trace.rs`. It plays the relay, the
  network, the adapters, the resolver and the fetch in virtual time.
- Tests of its own pin what the traces leave open: the order of deadlines due together, the ping schedule, a
  timer past 2^31 ms, the fetch's 5 s, a computed position past 2^53, the time rules above, and the close of
  a connection given up before it opens.
- The fuzz target `owp_party` feeds calls and answers, some with dead IDs. It checks for no panic, one `Done`
  per call, every `Open` closed, one command per lane, no effect on a dead ID, and no deadline left once
  `close` has run and every open effect has its answer.

`Party` is registered in `ci/machines.ini`, with two child machines, `Link` (one connection) and `Lane` (one
adapter's commands). `transition` does every write.
