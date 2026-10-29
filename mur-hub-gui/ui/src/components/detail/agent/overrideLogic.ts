/** Pure state for the override dialog: which flags are chosen and whether
 *  the typed name unlocks Continue. No I/O, so it is tested directly. */
export interface OverrideChoice {
  noHitl: boolean;
  netUnrestricted: boolean;
  fullPerms: boolean;
}

export const NO_CHOICE: OverrideChoice = { noHitl: false, netUnrestricted: false, fullPerms: false };

const ALL_CHOICE: OverrideChoice = { noHitl: true, netUnrestricted: true, fullPerms: true };

export function anyChosen(c: OverrideChoice): boolean {
  return c.noHitl || c.netUnrestricted || c.fullPerms;
}

export function allChosen(c: OverrideChoice): boolean {
  return c.noHitl && c.netUnrestricted && c.fullPerms;
}

/** "Select all": from all → none, from anything else → all. */
export function toggleAll(c: OverrideChoice): OverrideChoice {
  return allChosen(c) ? { ...NO_CHOICE } : { ...ALL_CHOICE };
}

/** Exact, case-sensitive match only — no trimming. */
export function canContinue(c: OverrideChoice, typed: string, name: string): boolean {
  return anyChosen(c) && typed === name;
}
