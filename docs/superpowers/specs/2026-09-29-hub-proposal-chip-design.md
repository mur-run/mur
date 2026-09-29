# Hub proposal chip (#1566)

The `propose` tool already renders in murmur (chip above the composer, Tab
inserts, Enter runs). The Hub dropped every `step/started`, so a proposal made
in a Hub conversation was invisible.

## Divergence from murmur

The Hub composer has **no `!` shell or `/` slash mode**: `ChatTab.send()` sends
whatever is typed to the agent as chat. Inserting `!mur open` would let the
user press Enter and send the command as a message — a trap. So in the Hub:

| kind | Hub action |
|---|---|
| shell / slash | show the command + **Copy**; nothing executes |
| restart | **Restart agent** → explicit confirm → `restart_agent` (stop, then start) |

Building a Hub-side shell runner was rejected: it is a new arbitrary-command
surface needing its own sandbox/permission design, out of scope here.

## Flow

1. `chat.rs` step callback → `proposal_chip::chip_from_step`, which re-runs
   `mur_common::proposal::vet` (same gate as murmur and the runtime). A
   rejected proposal is logged and never emitted.
2. Emits `proposal-offered` `{agent, label, kind, command?}`.
3. `ChatTab` filters on agent and feeds `chipReducer`
   (`proposalChipModel.ts`), rendered by `ProposalChip.tsx` in the existing
   `aboveCompose` slot position.

## State rules

- One chip per conversation; a newer offer replaces it.
- Send, Esc, ×, or switching agent dismisses it.
- `restart` can only start from the confirm step; while restarting, new offers
  and dismiss are ignored; failure keeps the chip with the error.
- If the Hub ever gains a reply ghost, it must never displace a chip.
