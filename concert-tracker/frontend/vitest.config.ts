import { defineConfig } from "vitest/config";

// Runs the Foldkit Story/Scene tests (foldkit's own MVU test harness), which
// need a DOM (snabbdom) and so run under happy-dom via vitest — separate from
// the pure node:test suites in ../../js-tests. Only *.story.test.ts /
// *.scene.test.ts are picked up here.
export default defineConfig({
  test: {
    environment: "happy-dom",
    setupFiles: ["./vitest-setup.ts"],
    include: ["src/**/*.{story,scene}.test.ts"],
    server: {
      deps: {
        // Foldkit and Effect ship ESM that vitest must transform in place.
        inline: ["foldkit", "effect", "@effect/platform-browser"],
      },
    },
  },
});
