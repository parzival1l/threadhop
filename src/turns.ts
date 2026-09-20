import type { Turn } from "./conversation.js";

export function selectLastTurns(turns: readonly Turn[], count: number): readonly Turn[] {
  if (!Number.isSafeInteger(count) || count < 1) {
    throw new RangeError("Turn count must be a positive safe integer.");
  }
  return turns.slice(-count);
}

export function renderTurns(turns: readonly Turn[]): string {
  return turns.flatMap((turn) => [
    `User:\n${turn.prompt.text}`,
    ...turn.responses.map((message) => `Assistant:\n${message.text}`),
  ]).join("\n\n");
}
