# pazari wire protocol

This is the normative specification of the protocol spoken between a pazari
guest agent (`pazari-agent`) and the pazari host daemon (`pazarid`).

The reference implementation is the `pazari-proto` crate. Where this document
names a constant (`PROTOCOL_VERSION`, `MAX_FRAME_BYTES`, `DEFAULT_VSOCK_PORT`),
the value is whatever that crate declares.

## Transport

The protocol is a bidirectional, ordered, reliable byte stream. The protocol itself makes no other demand
of the transport and carries no framing, addressing, or authentication of its
own: it assumes the transport already identifies the peer.

Each connection is one independent protocol session. Nothing is shared between
connections.

## Framing

Messages are newline-delimited JSON. Each message is a single JSON object,
serialised in UTF-8 with no interior line terminators, followed by one `LF`
(`0x0A`). There is no other whitespace between messages.

- A *line* is the bytes between terminators, excluding the terminating `LF`.
- A line MUST NOT exceed `MAX_FRAME_BYTES`, excluding its terminator. A line of
  exactly `MAX_FRAME_BYTES` is legal.
- `CR` (`0x0D`) is never a framing character. CRLF line endings are not
  accepted; a `CR` anywhere in a line is a protocol violation.
- Empty lines are not keepalives, comments, or padding. They are a protocol
  violation.

A sender MUST reject its own message rather than emit an over-long line. A
receiver MUST detect an over-long line during accumulation, that is, as soon as
`MAX_FRAME_BYTES` unterminated bytes have arrived, without waiting for the
terminator that would prove the sender is beyond the cap.

Framing is independent of the message payload format. `pazari-proto` expresses
this as a `Codec` trait, which declares `MAX_FRAME_BYTES`; `JsonLinesCodec` is
the initial implementation.

## Encoding conventions

**Tagging.** Every message is a JSON object with an internal discriminator
field named `type`, whose value is the message name in `snake_case`, for
example `grant_request`. The tag is the first member of the object as emitted,
though a receiver MUST NOT depend on member order.

**Memory quantities.** Every memory quantity is a count of bytes, encoded as a
JSON integer in the `u64` range.

**Durations.** Every duration is an integer count of milliseconds, and the
field name ends in `_ms`. The value MUST be a JSON integer in the `u64` range.

**Absolute values.** Every message is idempotent and carries absolute values.

**Optional fields.** A field documented as optional MAY be absent or explicitly
`null`; both mean "not supplied". Senders omit the field.

**Unknown fields.** A receiver MUST ignore object members it does not
recognise.

**Unknown messages.** An unknown `type` value is NOT forward-compatible and is
a protocol violation.

## Protocol version

There is no version negotiation. Each peer has one compiled-in
`PROTOCOL_VERSION` and accepts nothing else.

The guest opens the connection and sends `hello` carrying its
`PROTOCOL_VERSION` in `protocol`. The host closes the connection if that value
differs from its own constant. Otherwise it answers `welcome` carrying its own
`PROTOCOL_VERSION` in `protocol`, and the guest applies the same equality test
in the other direction, closing the connection on a mismatch.

There is no reject message. If the versions differ, or the guest is not a VM
the host manages, or the handshake is otherwise unacceptable, the host closes
the connection without sending anything. A guest that sees the connection close
before `welcome` MUST treat the host as unavailable and retry later; it MUST
NOT interpret the close as a grant, a denial, or a ceiling.

## Identifier semantics

There are two independent identifier spaces on a connection.

**Guest-initiated ids** appear in `grant_request` and `offer_release`. The
guest chooses them, monotonically increasing within the connection, and the
host answers each exactly once with a `grant` or a `deny` carrying the same
`id`.

**Host-initiated ids** appear in `reclaim_request`. The host chooses them,
monotonically increasing within the connection, and the guest answers each
exactly once with a `reclaim_ack` or a `reclaim_nak` carrying the same `id`.

Ids are connection-scoped and reset on every new connection. There is no
requirement that they start at any particular value, only that they increase.
On reconnect every in-flight request from the previous connection is dead: the
initiator MUST NOT expect an answer to it, and MUST re-issue the request under
a fresh id if it still wants the thing.

Replying twice to the same id, or replying to an id that was never issued, is a
protocol violation, and so is issuing a request with an id that is not greater
than the previous one in the same direction.

## Connection lifecycle

```
guest                                   host
  |------------- connect --------------->|
  |-------------- hello ---------------->|
  |<------------ welcome ----------------|
  |                                      |
  |--- grant_request / offer_release --->|
  |<--------- grant / deny --------------|
  |<-------- reclaim_request ------------|
  |----- reclaim_ack / reclaim_nak------>|
  |<----------- ceiling -----------------|
  |------------ status ----------------->|   every status_interval_ms
  |                                      |
  |-------------- bye ------------------>|
  |------------- close ----------------->|
```

1. **Connect.** The guest connects. The host accepts.
2. **Handshake.** The guest MUST send `hello` as its first message. The host
   MUST send `welcome` as its first message, in response to `hello`. Neither
   side sends anything else before the handshake completes.
3. **Steady state.** Either side may send any message its role permits, at any
   time, in any order, with any number of requests outstanding. The guest sends
   `status` every `status_interval_ms` as advertised in `welcome`; the host
   treats the absence of `status` as evidence the guest is gone. The host may
   send `ceiling` at any time.
4. **Close.** The guest sends `bye` and closes. `bye` is a courtesy; it lets
   the host distinguish an orderly shutdown from a crash. Either side may close
   at any time without it.

### Crash-only design

Neither side keeps durable protocol state. There is no session resumption, no
replay log, no acknowledgement of anything but requests, and nothing on the
wire that must survive a restart.

On reconnect each side re-derives its view from its own ground source and
starts talking again; it never needs to know what was said on the previous
connection.

The consequence is that **reconnect is the only recovery mechanism**, and it is
always sufficient.

## Error handling

Every one of the following is a protocol violation:

- a line longer than `MAX_FRAME_BYTES`
- an empty line
- a `CR` byte in a line
- a line that is not valid UTF-8
- a line that is not well-formed JSON
- a JSON value that is not an object, or an object with no `type` member
- an unknown `type`, or a `type` belonging to the other direction
- a field of the wrong JSON type, a missing required field, or a number outside
  its documented range

There is no error message and no in-band error reporting. A peer that detects a
violation MUST close the connection. It MUST NOT attempt to resynchronise by
skipping to the next line.

After a close, a guest reconnects with backoff and begins a fresh handshake.
A host waits to be reconnected to.

## Message catalogue

Types in the tables below are JSON types.

`uint` means a JSON integer in the
`u64` range unless stated otherwise; `bytes` means a `uint` counting bytes;
`ms` means a `uint` counting milliseconds.

### Guest to host

#### `hello`

The first message on every connection. Declares the agent's protocol version and
what the guest currently believes about its own memory geometry.

| Field           | Type      | Meaning                                                       |
| --------------- | --------- | ------------------------------------------------------------- |
| `type`          | `"hello"` | Message tag.                                                  |
| `protocol`      | uint      | The agent's compiled-in `PROTOCOL_VERSION`, `u16` range.      |
| `agent_version` | string    | Agent build version, for host-side logging and diagnostics.   |
| `boot_mem`      | bytes     | Memory the guest booted with, excluding anything hot-plugged. |
| `block_size`    | bytes     | The guest's virtio-mem block size.                            |
| `plugged`       | bytes     | Memory currently plugged in, as the guest sees it.            |

#### `grant_request`

Asks the host to raise plugged memory to `target`. Answered by exactly one
`grant` or `deny` with the same `id`.

| Field     | Type              | Meaning                                                       |
| --------- | ----------------- | ------------------------------------------------------------- |
| `type`    | `"grant_request"` | Message tag.                                                  |
| `id`      | uint              | Guest-chosen request id.                                      |
| `current` | bytes             | Plugged size at the time of the request.                      |
| `target`  | bytes             | Absolute plugged size wanted. Multiple of `block_size`.       |
| `reason`  | string enum       | Why the guest wants it: `throttle`, `preflight`, or `manual`. |
| `urgency` | string enum       | `low` or `high`.                                              |

`reason` values:

| Value       | Meaning                                                                                |
| ----------- | -------------------------------------------------------------------------------------- |
| `throttle`  | The workload is being braked by `memory.high` and is making poor progress.             |
| `preflight` | The agent expects to need the memory shortly and is asking before the workload stalls. |
| `manual`    | An operator or an external tool asked for it.                                          |

`urgency` values:

| Value  | Meaning                                                      |
| ------ | ------------------------------------------------------------ |
| `low`  | The workload is progressing; the request is an optimisation. |
| `high` | The workload is stalled or thrashing; latency matters.       |

#### `offer_release`

Offers to shrink. Answered by exactly one `grant` or `deny` with the same `id`;
a `grant` names the new lower plugged size.

| Field        | Type              | Meaning                                                 |
| ------------ | ----------------- | ------------------------------------------------------- |
| `type`       | `"offer_release"` | Message tag.                                            |
| `id`         | uint              | Guest-chosen request id.                                |
| `current`    | bytes             | Plugged size at the time of the offer.                  |
| `releasable` | bytes             | Bytes the guest has *already* freed and can unplug now. |

#### `reclaim_ack`

Accepts a host `reclaim_request`, in whole or in part.

| Field          | Type            | Meaning                                                                      |
| -------------- | --------------- | ---------------------------------------------------------------------------- |
| `type`         | `"reclaim_ack"` | Message tag.                                                                 |
| `id`           | uint            | The host's `reclaim_request` id being answered.                              |
| `will_release` | bytes           | Bytes the guest commits to free by the deadline. May be less than `desired`. |

#### `reclaim_nak`

Refuses a host `reclaim_request`.

| Field    | Type            | Meaning                                         |
| -------- | --------------- | ----------------------------------------------- |
| `type`   | `"reclaim_nak"` | Message tag.                                    |
| `id`     | uint            | The host's `reclaim_request` id being answered. |
| `reason` | string          | Free-form, human-readable explanation for logs. |

#### `status`

Unsolicited periodic telemetry, sent every `status_interval_ms`. Also the
heartbeat.

| Field            | Type       | Meaning                                                                 |
| ---------------- | ---------- | ----------------------------------------------------------------------- |
| `type`           | `"status"` | Message tag.                                                            |
| `plugged`        | bytes      | Memory currently plugged in, as the guest sees it.                      |
| `available`      | bytes      | `MemAvailable` from `/proc/meminfo`.                                    |
| `high_events`    | uint       | Monotonic `high` counter from the workload cgroup's `memory.events`.    |
| `max_events`     | uint       | Monotonic `max` counter from the workload cgroup's `memory.events`.     |
| `psi_some10`     | float      | Memory pressure `some` 10-second average, in percent, `0.0` to `100.0`. |
| `psi_full10`     | float      | Memory pressure `full` 10-second average, in percent, `0.0` to `100.0`. |
| `swap_used`      | bytes      | Swap in use.                                                            |
| `workload_usage` | bytes      | `memory.current` of the workload cgroup.                                |

#### `bye`

Graceful close.

| Field  | Type    | Meaning      |
| ------ | ------- | ------------ |
| `type` | `"bye"` | Message tag. |

### 8.2 Host to guest

#### `welcome`

The host's first message, in response to `hello`.

| Field                | Type        | Meaning                                                                                             |
| -------------------- | ----------- | --------------------------------------------------------------------------------------------------- |
| `type`               | `"welcome"` | Message tag.                                                                                        |
| `protocol`           | uint        | The host's compiled-in `PROTOCOL_VERSION`, `u16` range.                                             |
| `ceiling`            | bytes       | The maximum plugged size this guest may reach. A `grant_request` above it is denied with `ceiling`. |
| `status_interval_ms` | ms          | How often the guest must send `status`.                                                             |

#### `ceiling`

A runtime policy change of this guest's ceiling. Replaces the value from
`welcome` or from the previous `ceiling`.

| Field     | Type        | Meaning                                                                                                                                    |
| --------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `type`    | `"ceiling"` | Message tag.                                                                                                                               |
| `ceiling` | bytes       | The new maximum plugged size. May be below the current plugged size, in which case the host will normally follow with a `reclaim_request`. |

#### `grant`

Answers a `grant_request` or an `offer_release`. Means actuation toward `target` is underway.

| Field    | Type      | Meaning                                                                                                                                                                                                      |
| -------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `type`   | `"grant"` | Message tag.                                                                                                                                                                                                 |
| `id`     | uint      | The guest request id being answered.                                                                                                                                                                         |
| `target` | bytes     | The absolute plugged size the host is actuating toward. For a `grant_request` this may be less than requested. For an `offer_release` this is the new lower plugged size, and may release less than offered. |

#### `deny`

Answers a `grant_request` or an `offer_release` with a refusal.

| Field            | Type         | Meaning                                                                                                        |
| ---------------- | ------------ | -------------------------------------------------------------------------------------------------------------- |
| `type`           | `"deny"`     | Message tag.                                                                                                   |
| `id`             | uint         | The guest request id being answered.                                                                           |
| `reason`         | string enum  | `ceiling`, `host_pressure`, `rate_limited`, or `policy`.                                                       |
| `retry_after_ms` | ms, optional | Earliest time the guest should re-ask, as a delay from receipt. Absent or `null` means the host has no advice. |

`reason` values:

| Value           | Meaning                                                               |
| --------------- | --------------------------------------------------------------------- |
| `ceiling`       | The request exceeds this guest's ceiling.                             |
| `host_pressure` | The host does not have the memory right now. Worth retrying.          |
| `rate_limited`  | The guest is asking too often. Worth retrying after `retry_after_ms`. |
| `policy`        | An administrative rule forbids it. Retrying is unlikely to help.      |

#### `reclaim_request`

Asks the guest to free memory.

| Field         | Type                | Meaning                                               |
| ------------- | ------------------- | ----------------------------------------------------- |
| `type`        | `"reclaim_request"` | Message tag.                                          |
| `id`          | uint                | Host-chosen request id.                               |
| `desired`     | bytes               | Bytes the host would like freed.                      |
| `deadline_ms` | ms                  | How long the host will wait, as a delay from receipt. |
