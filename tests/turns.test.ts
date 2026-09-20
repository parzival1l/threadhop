import { describe, expect, it } from "vitest";
import type { Turn } from "../src/conversation.js";
import { renderTurns, selectLastTurns } from "../src/turns.js";

const session = { id: "session-1" };
const turns = [
  {
    session,
    prompt: { id: "u1", role: "user", text: "First question" },
    responses: [{ id: "a1", role: "assistant", text: "First answer" }],
  },
  {
    session,
    prompt: { id: "u2", role: "user", text: "Second question" },
    responses: [
      { id: "a2", role: "assistant", text: "Checking the files." },
      { id: "a3", role: "assistant", text: "Here is the answer." },
    ],
  },
] as const satisfies readonly Turn[];

describe("selectLastTurns", () => {
  it("selects a whole final turn, including every assistant response", () => {
    expect(selectLastTurns(turns, 1)).toEqual([turns[1]]);
  });

  it("keeps chronological order and leaves the input unchanged", () => {
    const frozen = Object.freeze([...turns]);
    expect(selectLastTurns(frozen, 2)).toEqual(turns);
    expect(frozen).toEqual(turns);
  });

  it("returns all available turns when the requested count is larger", () => {
    expect(selectLastTurns(turns, 10)).toEqual(turns);
    expect(selectLastTurns([], 1)).toEqual([]);
  });

  it.each([0, -1, 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1])(
    "rejects an invalid turn count %s",
    (count) => expect(() => selectLastTurns(turns, count)).toThrow(RangeError),
  );
});

describe("renderTurns", () => {
  it("renders the selected prompt and all responses in order", () => {
    expect(renderTurns(selectLastTurns(turns, 1))).toBe(
      "User:\nSecond question\n\nAssistant:\nChecking the files.\n\nAssistant:\nHere is the answer.",
    );
  });

  it("renders a pending turn without inventing an assistant reply", () => {
    expect(renderTurns([{ session, prompt: turns[1].prompt, responses: [] }])).toBe(
      "User:\nSecond question",
    );
  });

  it("renders an empty conversation as empty text", () => {
    expect(renderTurns([])).toBe("");
  });
});
