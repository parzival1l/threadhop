import { describe, expect, it } from "vitest";
import { sessionLabel } from "../src/session-label.js";

describe("sessionLabel", () => {
  it("uses the session title without surrounding whitespace", () => {
    expect(sessionLabel("session-123", "  Investigate search  ")).toBe(
      "Investigate search",
    );
  });

  it("falls back to the full session ID when no title is available", () => {
    expect(sessionLabel("session-123")).toBe("session-123");
  });

  it.each(["", " \t\n "])("falls back for a blank title %j", (title) => {
    expect(sessionLabel("session-123", title)).toBe("session-123");
  });
});
