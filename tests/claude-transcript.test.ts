import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { parseClaudeTranscript, readClaudeTranscript } from "../src/claude-transcript.js";
import { renderTurns, selectLastTurns } from "../src/turns.js";

const fixture = fileURLToPath(new URL("./fixtures/claude-session.jsonl", import.meta.url));
const session = { id: "fallback" };
const user = (uuid: string, content: unknown, extra = {}) => ({
  type: "user", uuid, message: { content }, ...extra,
});
const assistant = (uuid: string, text: string, extra = {}) => ({
  type: "assistant", uuid, message: { id: uuid, content: [{ type: "text", text }] }, ...extra,
});
const jsonl = (...records: unknown[]) => records.map((record) => JSON.stringify(record)).join("\n") + "\n";

describe("Claude transcript reading", () => {
  it("reads two turns, excludes tool/subagent output, and merges chunks without duplicating records", async () => {
    const result = await Effect.runPromise(readClaudeTranscript(fixture));
    expect(result.turns).toHaveLength(2);
    expect(result.turns[1]?.session.id).toBe("demo-session");
    expect(result.turns[1]?.responses.map((message) => message.id)).toEqual(["m2", "m3"]);
    expect(renderTurns(selectLastTurns(result.turns, 1))).toBe(
      "User:\nWhy is search slow?\n\nAssistant:\nI will check the index.\n\nAssistant:\nThe index\n\nis being rebuilt.",
    );
  });

  it("does not let tool-result or synthetic user rows split a human turn", () => {
    const result = parseClaudeTranscript(jsonl(
      user("u1", "Question"), assistant("a1", "First answer"),
      user("tool", [{ type: "tool_result", content: "tool output" }, { type: "text", text: "tool metadata" }]),
      user("meta", "Injected instruction", { isMeta: true }),
      user("sub", "Subagent task", { agentId: "worker-1" }),
      assistant("a2", "Final answer"),
    ), session);
    expect(result.turns).toHaveLength(1);
    expect(result.turns[0]?.responses.map((message) => message.text)).toEqual(["First answer", "Final answer"]);
  });

  it("cleans harness wrappers and excludes loaded skills without losing a real prompt", () => {
    const result = parseClaudeTranscript(jsonl(
      user("u1", "<system-reminder>hidden</system-reminder>Actual question"),
      user("cmd", "<bash-input>threadhop copy</bash-input><bash-stdout>copied</bash-stdout>"),
      user("skill", "Base directory for this skill: /example/skill\nInjected skill text"),
      assistant("a1", "<system-reminder>hidden</system-reminder>Answer"),
    ), session);
    expect(renderTurns(result.turns)).toBe("User:\nActual question\n\nAssistant:\nAnswer");
  });

  it("keeps earlier complete turns when the final record is incomplete", () => {
    const result = parseClaudeTranscript(jsonl(user("u1", "Question"), assistant("a1", "Answer")) + '{"type":"user",', session);
    expect(renderTurns(result.turns)).toBe("User:\nQuestion\n\nAssistant:\nAnswer");
    expect(result.diagnostics).toContainEqual({ line: 3, reason: "Incomplete final JSONL record" });
  });

  it("accepts a complete last record without a trailing newline", () => {
    const result = parseClaudeTranscript(jsonl(user("u1", "Question"), assistant("a1", "Answer")).trimEnd(), session);
    expect(result.turns[0]?.responses[0]?.text).toBe("Answer");
    expect(result.diagnostics).toEqual([]);
  });

  it("reports malformed, invalid, and unknown records while continuing", () => {
    const text = jsonl(user("u1", "Question")) + 'not json\nnull\n{"type":"future_event"}\n' + jsonl(assistant("a1", "Answer"));
    const result = parseClaudeTranscript(text, session);
    expect(result.turns[0]?.responses[0]?.text).toBe("Answer");
    expect(result.diagnostics.map((item) => item.line)).toEqual([2, 3, 4]);
  });

  it("does not merge a different session into the supplied transcript", () => {
    const result = parseClaudeTranscript(jsonl(
      user("u1", "Question", { sessionId: "s1" }),
      assistant("other", "Other session", { sessionId: "s2" }),
      assistant("a1", "Answer", { sessionId: "s1" }),
    ), session);
    expect(result.turns[0]?.responses.map((message) => message.text)).toEqual(["Answer"]);
    expect(result.diagnostics[0]?.reason).toMatch(/session/i);
  });

  it("reports a text block with no text instead of silently discarding it", () => {
    const result = parseClaudeTranscript(jsonl(
      user("u1", "Question"),
      { type: "assistant", uuid: "bad", message: { content: [{ type: "text" }] } },
      assistant("a1", "Answer"),
    ), session);
    expect(result.diagnostics).toContainEqual({ line: 2, reason: "Invalid conversation record" });
    expect(result.turns[0]?.responses[0]?.text).toBe("Answer");
  });

  it("preserves a pending prompt, repeated human text, and a fallback session ID", () => {
    const result = parseClaudeTranscript(jsonl(user("u1", "Again"), user("u2", "Again")), session);
    expect(result.turns.map((turn) => turn.prompt.id)).toEqual(["u1", "u2"]);
    expect(result.turns[1]?.responses).toEqual([]);
    expect(result.turns[1]?.session.id).toBe("fallback");
  });

  it("does not manufacture a human prompt for leading assistant output", () => {
    const result = parseClaudeTranscript(jsonl(assistant("a1", "Resumed output")), session);
    expect(result.turns).toEqual([]);
    expect(result.diagnostics).toHaveLength(1);
  });

  it.each([true, false])("keeps an attachment-only user turn with reply=%s", (hasReply) => {
    const records = [
      user("u1", "First question"), assistant("a1", "First answer"),
      user("u2", [{ type: "image", source: { type: "base64", data: "omitted" } }]),
      ...(hasReply ? [assistant("a2", "The image has a cat.")] : []),
    ];
    const result = parseClaudeTranscript(jsonl(...records), session);
    expect(result.turns).toHaveLength(2);
    expect(result.turns[1]?.prompt.id).toBe("u2");
    expect(result.turns[1]?.prompt.text).toBe("[User attachment; content omitted]");
    expect(result.turns[1]?.responses.map((message) => message.text)).toEqual(
      hasReply ? ["The image has a cat."] : [],
    );
    expect(renderTurns(selectLastTurns(result.turns, 1))).not.toContain("First question");
    expect(result.diagnostics).toContainEqual({ line: 3, reason: "Non-text user content omitted" });
  });

  it("fails clearly for an unreadable path", async () => {
    await expect(Effect.runPromise(readClaudeTranscript(`${fixture}.missing`))).rejects.toThrow(/Cannot read transcript/);
  });

  it("returns empty results for an empty transcript", () => {
    expect(parseClaudeTranscript("", session)).toEqual({ turns: [], diagnostics: [] });
  });

  it("does not modify the source file", async () => {
    const before = await readFile(fixture, "utf8");
    await Effect.runPromise(readClaudeTranscript(fixture));
    expect(await readFile(fixture, "utf8")).toBe(before);
  });
});
