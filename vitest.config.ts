import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/**/*.test.ts"],
    forceRerunTriggers: ["src/**/*.ts", "tsconfig*.json", "vitest.config.ts", "package.json"],
  },
});
