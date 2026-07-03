import { defineConfig } from "vitest/config";

// Runs the Foldkit Story/Scene tests (foldkit's own MVU test harness), which
// need a DOM (snabbdom) and so run under happy-dom via vitest — separate from
// the pure node:test suites in ../../js-tests. *.story.test.ts / *.scene.test.ts
// cover Message-level and view-level behavior via Foldkit's test harness, which
// never runs a Command's real Effect body (Commands are resolved abstractly).
// *.command.test.ts is for the rare Command whose Effect has DOM-dependent
// branching (e.g. an element being present/absent) that only a real Effect
// run against happy-dom can exercise. *.unit.test.ts is for plain
// Element-argument predicates (e.g. player/core.ts's keyboard-target
// helpers) that need a DOM fixture but aren't Story/Scene/Command tests.
export default defineConfig({
  test: {
    environment: "happy-dom",
    setupFiles: ["./vitest-setup.ts"],
    include: ["src/**/*.{story,scene,command,unit}.test.ts"],
    server: {
      deps: {
        // Foldkit and Effect ship ESM that vitest must transform in place.
        inline: ["foldkit", "effect", "@effect/platform-browser"],
      },
    },
  },
});
