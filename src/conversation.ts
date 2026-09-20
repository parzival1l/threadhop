import { Schema } from "effect";

// Small text-facing values for peek, not a schema for native transcript records.
export const SessionReference = Schema.Struct({
  id: Schema.NonEmptyTrimmedString,
  title: Schema.optional(Schema.String),
});
export type SessionReference = typeof SessionReference.Type;

export const UserMessage = Schema.Struct({
  id: Schema.NonEmptyTrimmedString,
  role: Schema.Literal("user"),
  text: Schema.String,
});
export type UserMessage = typeof UserMessage.Type;

export const AssistantMessage = Schema.Struct({
  id: Schema.NonEmptyTrimmedString,
  role: Schema.Literal("assistant"),
  text: Schema.String,
});
export type AssistantMessage = typeof AssistantMessage.Type;

export const Message = Schema.Union(UserMessage, AssistantMessage);
export type Message = typeof Message.Type;

export const Turn = Schema.Struct({
  session: SessionReference,
  prompt: UserMessage,
  responses: Schema.Array(AssistantMessage),
});
export type Turn = typeof Turn.Type;

// Accepts unknown input and returns Either<validated Turn, ParseError>.
// Inferred properties/arrays are readonly; decoding does not deep-freeze them.
export const decodeTurn = Schema.decodeUnknownEither(Turn);
