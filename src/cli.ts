import { parseArgs } from "node:util";
import { Effect, Either } from "effect";
import { readClaudeTranscript } from "./claude-transcript.js";
import { renderTurns, selectLastTurns } from "./turns.js";

const usage = "Usage: threadhop-next peek <transcript.jsonl> [--last N]\n\nRead the last N main-agent turns (default: 1). Use --help for this message.";

async function main(args: string[]): Promise<number> {
  let path: string;
  let count: number;
  try {
    const { values, positionals } = parseArgs({
      args,
      allowPositionals: true,
      strict: true,
      options: {
        last: { type: "string", default: "1" },
        help: { type: "boolean", short: "h" },
      },
    });
    if (values.help || args.length === 0) {
      console.log(usage);
      return 0;
    }
    const [command, file] = positionals;
    if (command !== "peek" || !file || positionals.length !== 2) {
      throw new Error("Expected peek and exactly one transcript path.");
    }
    count = Number(values.last);
    if (!/^[1-9]\d*$/.test(values.last) || !Number.isSafeInteger(count)) {
      throw new Error("--last must be a positive safe integer.");
    }
    path = file;
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    console.error(usage);
    return 2;
  }

  const result = await Effect.runPromise(Effect.either(readClaudeTranscript(path)));
  if (Either.isLeft(result)) {
    console.error(result.left.message);
    return 1;
  }
  const { turns, diagnostics } = result.right;
  const first = diagnostics[0];
  if (first) {
    console.error(`Skipped ${diagnostics.length} record(s); first at line ${first.line}: ${first.reason}`);
  }
  if (turns.length === 0) {
    console.error("No main-agent turns found in this transcript.");
    return 1;
  }
  console.log(renderTurns(selectLastTurns(turns, count)));
  return 0;
}

process.exitCode = await main(process.argv.slice(2));
