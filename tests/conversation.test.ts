import { Either, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { Message, SessionReference, Turn, decodeTurn } from "../src/conversation.js";

const sampleTurn = {
  session: { id: "session-1", title: "Investigate search" },
  prompt: { id: "message-1", role: "user", text: "Why is search slow?" },
  responses: [
    { id: "message-2", role: "assistant", text: "I will check the index." },
    { id: "message-3", role: "assistant", text: "The index is being rebuilt." },
  ],
} satisfies Turn;

describe("conversation values", () => {
  it("accepts a session reference without a title", () => {
    expect(Schema.decodeUnknownSync(SessionReference)({ id: "session-1" })).toEqual({
      id: "session-1",
    });
  });

  it("validates one human prompt and preserves multiple assistant responses in order", () => {
    expect(decodeTurn(sampleTurn)).toEqual(Either.right(sampleTurn));
  });

  it("accepts a turn whose assistant has not replied yet", () => {
    const pending = { ...sampleTurn, responses: [] };
    expect(decodeTurn(pending)).toEqual(Either.right(pending));
  });

  it("preserves message text, including code indentation", () => {
    const message = { id: "message-4", role: "assistant", text: "  return value;\n" };
    expect(Schema.decodeUnknownSync(Message)(message)).toEqual(message);
  });

  it.each([
    ["an assistant prompt", { ...sampleTurn, prompt: sampleTurn.responses[0] }],
    ["a user in the responses", { ...sampleTurn, responses: [sampleTurn.prompt] }],
    ["a missing prompt", { session: sampleTurn.session, responses: [] }],
    ["an empty session ID", { ...sampleTurn, session: { id: "" } }],
    ["a blank message ID", { ...sampleTurn, prompt: { ...sampleTurn.prompt, id: " " } }],
    ["non-text content", { ...sampleTurn, prompt: { ...sampleTurn.prompt, text: 42 } }],
  ])("rejects %s as a turn", (_reason, input) => {
    expect(Either.isLeft(decodeTurn(input))).toBe(true);
  });

  it("does not accept a tool result as a human or assistant message", () => {
    const result = Schema.decodeUnknownEither(Message)({
      id: "tool-result-1",
      role: "tool_result",
      text: "Command output",
    });
    expect(Either.isLeft(result)).toBe(true);
  });
});
