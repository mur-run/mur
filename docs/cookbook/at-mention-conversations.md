# `@` mentions: addressing an agent's conversation

A chat with an agent is a numbered **channel**, and that channel's signed log is
the only record of the conversation. The agent rebuilds its context from the
log on every turn, keyed by channel id. So a conversation continues the same
way after the runtime restarts, after its cache evicts the thread, and after
`mur channel repair`. A log that cannot be trusted refuses the turn by name.
It never falls back silently to a stale copy.

## Addressing

```bash
mur agent send '@coach#3' 'and the second option?'   # continue conversation #3
mur agent send '@coach' 'start fresh or continue'    # coach's latest conversation, or a new one
mur agent send '@agent:coach#3' '…'                  # explicit namespace
```

| Form | Meaning |
|---|---|
| `@name text` | the agent (or fleet) named `name`; agent names match case-insensitively |
| `@name#N text` | that agent's conversation `#N` (the number `/channels` shows) |
| `@agent:name[#N]`, `@fleet:name` | explicit namespace; needed when one name is both an agent and a fleet |

`agent`, `fleet` and `all` are reserved and cannot name a target. `#N` must
be `1` or more, and a fleet takes no `#N`. Fleet delivery is not available
yet: `@fleet:name` resolves, but is refused with `fleet targets are Phase 4`.

`mur agent send '@coach#N'` writes your message into channel `#N` as coach,
then dials coach with that channel and the message's position in the log. It
prints `→ coach#N (channel <id>, seq <seq>)` on stderr. Without `#N`, it uses
coach's latest conversation and creates one only if coach has none. When a
target is refused, the command prints the code and exits non-zero. A refused
send never creates a channel and never adds a participant.

### In murmur

A murmur pane belongs to one agent, and its channel events are signed with
that agent's key. So `@` inside murmur only addresses the pane's own agent:

- `@coach#3 text` switches the pane to conversation `#3` and sends `text`.
  A bare `@coach#3` only switches.
- `@coach text` sends `text` in the current conversation.
- `@other …` sends nothing and shows
  `use: mur agent send '@other' … — this pane is bound to coach`.

`/channels N` switches by number too. Use `@coach#N text` to switch and send
in one line. The status bar shows the conversation as `coach#N · <id>`, which
is the address you would type.

## Refusals

Addressing refusals come from `mur agent send` and murmur before anything is
written:

| Code | When |
|---|---|
| `RESERVED_NAME` | the target is `agent`, `fleet` or `all` |
| `UNKNOWN_TARGET` | no agent or fleet has that name |
| `AMBIGUOUS_TARGET` | the name is both an agent and a fleet; write `@agent:` or `@fleet:` |
| `UNKNOWN_CONVERSATION` | there is no channel `#N` |
| `CHANNEL_NOT_CONVERSATION` | channel `#N` is a fleet or workflow run |
| `NOT_A_PARTICIPANT` | the agent is not a member of channel `#N` |
| `TARGET_OFFLINE` | the agent is not running; `mur agent start <agent>` |

The agent itself refuses a channel turn with one of these JSON-RPC errors.
They are checked in this order, and the first failure wins:

| JSON-RPC code | Message prefix | When |
|---|---|---|
| -32020 | `unknown conversation:` | no channel with that id on this host |
| -32021 | `channel too new:` | the channel was written by a newer MUR; update |
| -32022 | `channel is not a conversation:` | the channel is a fleet or workflow run |
| -32023 | `not a participant:` | the agent is not a member |
| -32024 | `conversation repairing:` | a `mur channel repair` is in progress; retry when it finishes |
| -32025 | `conversation damaged:` | the log cannot be trusted; `data.reason` says why (below) |
| -32026 | `conversation busy:` | too many turns already wait on this conversation |

`conversation damaged:` carries one `data.reason` value:

| `data.reason` | Meaning |
|---|---|
| `manifest` | the channel manifest is missing or unreadable |
| `unparseable` | log lines do not parse; `data.lines` lists them |
| `signature` | an event's signature does not verify |
| `chain` | the hash chain is broken |
| `anchor` | the log does not match its signed head |
| `log-regressed` | the log is shorter than the agent last saw, and no repair explains it |
| `watermark` | the message named by the turn is not in the log as sent |

murmur shows these refusals as denials and does not retry the turn, because
the same turn would hit the same refusal. A damaged conversation is fixed with
`mur channel repair <channel>`. After the repair, the agent continues from the
fresh chain.

## Configuration

```yaml
# ~/.mur/config.yaml
mention:
  conversation_max_waiting: 4   # turns that may queue on one conversation before `conversation busy:`
```

Turns in one conversation run one at a time, in order. Turns in different
conversations do not wait on each other.
