/** What the Home peek can show (spec 3(b) §3). */
export type PeekTarget = { kind: "chat"; agent: string } | { kind: "fleet"; name: string };

export const FLEET_CHANNEL_PREFIX = "fleet-";

/** A fleet's channel → that fleet; an agent's primary channel (id == agent
 *  name) → that chat; anything else → null, and the caller keeps today's
 *  navigation. */
export function peekTargetForChannel(channel: { id: string }, agentNames: ReadonlySet<string>): PeekTarget | null {
  if (channel.id.startsWith(FLEET_CHANNEL_PREFIX)) {
    const name = channel.id.slice(FLEET_CHANNEL_PREFIX.length);
    return name ? { kind: "fleet", name } : null;
  }
  if (agentNames.has(channel.id)) return { kind: "chat", agent: channel.id };
  return null;
}
