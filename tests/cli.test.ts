import { execFileSync, spawnSync } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

const cli = fileURLToPath(new URL("../dist/cli.js", import.meta.url));
const fixture = fileURLToPath(new URL("./fixtures/claude-session.jsonl", import.meta.url));
const temporaryDirectories: string[] = [];
// Build on each suite run, including watch mode, to avoid testing stale output.
beforeAll(() => {
  execFileSync(process.execPath, [
    fileURLToPath(new URL("../node_modules/typescript/bin/tsc", import.meta.url)),
    "--project", "tsconfig.build.json",
  ], { cwd: fileURLToPath(new URL("..", import.meta.url)), stdio: "pipe", timeout: 15_000 });
});
const run = (...args: string[]) => spawnSync(process.execPath, [cli, ...args], {
  encoding: "utf8", timeout: 10_000,
});
afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("peek CLI", () => {
  it("prints the final turn by default through the real executable", () => {
    const result = run("peek", fixture);
    expect(result.status).toBe(0);
    expect(result.stderr).toBe("");
    expect(result.stdout).toBe(
      "User:\nWhy is search slow?\n\nAssistant:\nI will check the index.\n\nAssistant:\nThe index\n\nis being rebuilt.\n",
    );
  });

  it("supports an explicit turn count", () => {
    const result = run("peek", fixture, "--last", "2");
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("User:\nWhat is ThreadHop?");
    expect(result.stdout.indexOf("What is ThreadHop?")).toBeLessThan(result.stdout.indexOf("Why is search slow?"));
  });

  it("prints help without reading a transcript", () => {
    const result = run("--help");
    expect(result.status).toBe(0);
    expect(result.stdout).toContain("peek <transcript.jsonl>");
    expect(result.stderr).toBe("");
  });

  it.each(["0", "-1", "1.5", "abc", "1e2", "9007199254740992"])(
    "rejects invalid --last value %s", (count) => {
      const result = run("peek", fixture, `--last=${count}`);
      expect(result.status).toBe(2);
      expect(result.stderr).toContain("positive safe integer");
      expect(result.stdout).toBe("");
    },
  );

  it.each([
    ["peek"], ["other", fixture], ["peek", fixture, "--unknown"],
    ["peek", fixture, "--last"], ["peek", fixture, "extra-path"],
  ])("rejects invalid command arguments %j", (...args) => {
    const result = run(...args);
    expect(result.status).toBe(2);
    expect(result.stderr).toContain("Usage:");
    expect(result.stdout).toBe("");
  });

  it("reports missing files without a stack trace on stdout", () => {
    const result = run("peek", `${fixture}.missing`);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("Cannot read transcript");
    expect(result.stdout).toBe("");
  });

  it("reports an empty transcript", async () => {
    const dir = await mkdtemp(join(tmpdir(), "threadhop-cli-"));
    temporaryDirectories.push(dir);
    const path = join(dir, "empty.jsonl");
    await writeFile(path, "");
    const result = run("peek", path);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("No main-agent turns");
    expect(result.stdout).toBe("");
  });

  it("prints a pending prompt from a path containing spaces and puts parse diagnostics on stderr", async () => {
    const dir = await mkdtemp(join(tmpdir(), "threadhop-cli-"));
    temporaryDirectories.push(dir);
    const path = join(dir, "pending session.jsonl");
    await writeFile(path, '{"type":"user","uuid":"u1","message":{"content":"Pending question"}}\n{"type":');
    const result = run("peek", path, "--last", "1");
    expect(result.status).toBe(0);
    expect(result.stdout).toBe("User:\nPending question\n");
    expect(result.stderr).toContain("line 2");
  });
});
