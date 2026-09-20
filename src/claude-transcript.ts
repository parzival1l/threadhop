import { readFile } from "node:fs/promises";
import { basename, extname } from "node:path";
import { Data, Effect, Either, Schema } from "effect";
import type { AssistantMessage, SessionReference, Turn, UserMessage } from "./conversation.js";

const Envelope = Schema.Struct({
  type: Schema.String,
  isSidechain: Schema.optional(Schema.Boolean),
  agentId: Schema.optional(Schema.String),
  isMeta: Schema.optional(Schema.Boolean),
});
const ContentBlock = Schema.Struct({
  type: Schema.String,
  text: Schema.optional(Schema.String),
}).pipe(Schema.filter((block) => block.type !== "text" || block.text !== undefined));
const ConversationRecord = Schema.Struct({
  ...Envelope.fields,
  uuid: Schema.NonEmptyTrimmedString,
  sessionId: Schema.optional(Schema.NonEmptyTrimmedString),
  toolUseResult: Schema.optional(Schema.Unknown),
  message: Schema.Struct({
    id: Schema.optional(Schema.NonEmptyTrimmedString),
    role: Schema.optional(Schema.Literal("user", "assistant")),
    content: Schema.Union(Schema.String, Schema.Array(ContentBlock)),
  }),
});
const decodeEnvelope = Schema.decodeUnknownEither(Envelope);
const decodeRecord = Schema.decodeUnknownEither(ConversationRecord);

// Same cleaning policy as the Python copy/exchange pipeline. This affects the
// displayed text only; source files are never rewritten.
function cleanText(text: string, role: "user" | "assistant"): string {
  let cleaned = text.replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, "");
  cleaned = cleaned.replace(
    /<(bash-input|bash-stdout|bash-stderr|local-command-caveat|command-name|command-message|command-args)>[\s\S]*?<\/\1>/g,
    "",
  );
  if (role === "user") {
    cleaned = cleaned.replace(/<local-command-(stdout|stderr)>[\s\S]*?<\/local-command-\1>/g, "");
    if (/^\s*Base directory for this skill:\s*\S+/.test(cleaned)) return "";
  }
  return cleaned.trim();
}

export interface TranscriptDiagnostic {
  readonly line: number;
  readonly reason: string;
}
export interface TranscriptResult {
  readonly turns: readonly Turn[];
  readonly diagnostics: readonly TranscriptDiagnostic[];
}

export function parseClaudeTranscript(text: string, fallbackSession: SessionReference): TranscriptResult {
  const turns: Array<{ session: SessionReference; prompt: UserMessage; responses: AssistantMessage[] }> = [];
  const diagnostics: TranscriptDiagnostic[] = [];
  const seenRecordIds = new Set<string>();
  let nativeSessionId: string | undefined;
  const lines = text.split("\n");

  for (const [index, line] of lines.entries()) {
    if (!line.trim()) continue;
    const report = (reason: string) => diagnostics.push({ line: index + 1, reason });
    let raw: unknown;
    try {
      raw = JSON.parse(line);
    } catch {
      report(index === lines.length - 1 ? "Incomplete final JSONL record" : "Malformed JSONL record");
      continue;
    }
    const envelope = decodeEnvelope(raw);
    if (Either.isLeft(envelope)) {
      report("Invalid record envelope");
      continue;
    }
    if (envelope.right.isSidechain || envelope.right.agentId !== undefined || envelope.right.isMeta) continue;
    const role = envelope.right.type;
    if (role !== "user" && role !== "assistant") {
      report(`Ignored event type: ${role}`);
      continue;
    }
    const decoded = decodeRecord(raw);
    if (Either.isLeft(decoded)) {
      report("Invalid conversation record");
      continue;
    }
    const record = decoded.right;
    if (record.message.role !== undefined && record.message.role !== role) {
      report("Record type and message role disagree");
      continue;
    }
    if (record.sessionId !== undefined) {
      nativeSessionId ??= record.sessionId;
      if (record.sessionId !== nativeSessionId) {
        report("Ignored record from another session");
        continue;
      }
    }
    if (seenRecordIds.has(record.uuid)) continue;
    seenRecordIds.add(record.uuid);
    const content = record.message.content;
    if (role === "user" && (record.toolUseResult != null ||
      (typeof content !== "string" && content.some((block) => block.type === "tool_result")))) continue;

    const prose = typeof content === "string"
      ? content
      : content.filter((block) => block.type === "text").map((block) => block.text ?? "").join("\n\n");
    let cleaned = cleanText(prose, role);
    if (!cleaned && role === "user" && typeof content !== "string" &&
      content.some((block) => block.type === "image" || block.type === "document")) {
      cleaned = "[User attachment; content omitted]";
      report("Non-text user content omitted");
    }
    if (!cleaned) continue;

    if (role === "user") {
      turns.push({
        session: { ...fallbackSession, id: nativeSessionId ?? fallbackSession.id },
        prompt: { id: record.uuid, role, text: cleaned },
        responses: [],
      });
      continue;
    }
    const current = turns.at(-1);
    if (!current) {
      report("Assistant text before the first human prompt was skipped");
      continue;
    }
    const id = record.message.id ?? record.uuid;
    const previous = current.responses.at(-1);
    if (previous?.id === id) {
      current.responses[current.responses.length - 1] = {
        ...previous, text: `${previous.text}\n\n${cleaned}`,
      };
    } else {
      current.responses.push({ id, role, text: cleaned });
    }
  }
  return { turns, diagnostics };
}

export class TranscriptReadError extends Data.TaggedError("TranscriptReadError")<{
  readonly path: string;
  readonly message: string;
}> {}

export const readClaudeTranscript = (path: string) => Effect.tryPromise({
  try: () => readFile(path, "utf8"),
  catch: (cause) => new TranscriptReadError({
    path,
    message: `Cannot read transcript ${path}: ${cause instanceof Error ? cause.message : String(cause)}`,
  }),
}).pipe(Effect.map((text) => parseClaudeTranscript(text, {
  id: basename(path, extname(path)).trim() || "session",
})));
