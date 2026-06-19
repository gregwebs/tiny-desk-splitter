// Unit tests for the pure editor logic in concert-tracker/frontend/src/splitter/core.ts.
// Run with: npm run test:unit (node --import tsx --test js-tests/*.test.ts)
// These cover only the DOM-free logic; the DOM and interaction layer (./index.ts)
// is exercised by the Playwright e2e suite under ./e2e.
import test from "node:test";
import assert from "node:assert/strict";

import * as P from "../concert-tracker/frontend/src/splitter/core";
import type { SplitTimestampsResponse } from "../concert-tracker/frontend/src/api/client";

const close = (a: number, b: number, eps = 1e-9) => Math.abs(a - b) <= eps;

test("parseTimecode parses mm:ss(.s), h:mm:ss, and bare seconds", () => {
  assert.equal(P.parseTimecode("2:05"), 125);
  assert.ok(close(P.parseTimecode("2:05.5"), 125.5));
  assert.equal(P.parseTimecode("1:00:00"), 3600);
  assert.equal(P.parseTimecode("90"), 90);
  assert.ok(close(P.parseTimecode("90.25"), 90.25));
});

test("parseTimecode rejects garbage as NaN", () => {
  for (const bad of ["", "  ", "abc", "1:bad", "1:2:3:4", ":", "1::2"]) {
    assert.ok(Number.isNaN(P.parseTimecode(bad)), `expected NaN for ${JSON.stringify(bad)}`);
  }
  assert.ok(Number.isNaN(P.parseTimecode(undefined)));
  assert.ok(Number.isNaN(P.parseTimecode(125)));
});

test("formatTimecode renders one decimal and pads seconds", () => {
  assert.equal(P.formatTimecode(125.5), "2:05.5");
  assert.equal(P.formatTimecode(5), "0:05.0");
  assert.equal(P.formatTimecode(0), "0:00.0");
  assert.equal(P.formatTimecode(-3), "0:00.0");
});

function sampleResp(): SplitTimestampsResponse {
  return {
    set_list: ["A", "B", "C"],
    auto: [
      { title: "A", start_time: 0, end_time: 100, duration: 100 },
      { title: "B", start_time: 100, end_time: 200, duration: 100 },
      { title: "C", start_time: 200, end_time: 290, duration: 90 },
    ],
    user: null,
    media_duration: 300,
  };
}

test("initState prefers user over auto and detects linked boundaries", () => {
  const resp = sampleResp();
  const st = P.initState(resp);
  assert.ok(st);
  assert.equal(st.tracks.length, 3);
  assert.equal(st.duration, 300);
  assert.deepEqual(st.linked, [true, true]); // contiguous auto split points

  // A user split with a gap between A and B is detected as detached.
  resp.user = [
    { title: "A", start_time: 0, end_time: 90, duration: 90 },
    { title: "B", start_time: 110, end_time: 200, duration: 90 },
    { title: "C", start_time: 200, end_time: 290, duration: 90 },
  ];
  const st2 = P.initState(resp);
  assert.ok(st2);
  assert.deepEqual(st2.linked, [false, true]);
});

test("initState returns null with no timestamps and falls back on duration", () => {
  assert.equal(P.initState({ set_list: [], auto: null, user: null, media_duration: null }), null);
  const st = P.initState({
    set_list: ["A"],
    auto: [{ title: "A", start_time: 0, end_time: 180, duration: 180 }],
    user: null,
    media_duration: null,
  });
  assert.ok(st);
  assert.equal(st.duration, 180); // falls back to last end time
});

test("setEnd on a linked boundary moves the next track's start too", () => {
  const st = P.initState(sampleResp());
  assert.ok(st);
  P.setEnd(st, 0, 120);
  assert.equal(st.tracks[0]!.end, 120);
  assert.equal(st.tracks[1]!.start, 120); // dragged together
});

test("setEnd clamps to neighbour and minimum segment", () => {
  const st = P.initState(sampleResp());
  assert.ok(st);
  // Cannot push track 0's end past where it would leave <1s for track 1.
  P.setEnd(st, 0, 500);
  assert.equal(st.tracks[0]!.end, st.tracks[1]!.end - P.MIN_SEG); // 200 - 1
  assert.equal(st.tracks[1]!.start, st.tracks[0]!.end);
  // Cannot shrink below MIN_SEG.
  P.setStart(st, 0, 0);
  P.setEnd(st, 0, -10);
  assert.ok(close(st.tracks[0]!.end, st.tracks[0]!.start + P.MIN_SEG));
});

test("detach then drag opens a gap; link collapses it", () => {
  const st = P.initState(sampleResp());
  assert.ok(st);
  P.detach(st, 0);
  assert.equal(st.linked[0], false);
  // Now end[0] and start[1] move independently.
  P.setEnd(st, 0, 90);
  P.setStart(st, 1, 110);
  assert.equal(st.tracks[0]!.end, 90);
  assert.equal(st.tracks[1]!.start, 110); // 20s gap
  // Detached end cannot cross the next start.
  P.setEnd(st, 0, 200);
  assert.equal(st.tracks[0]!.end, st.tracks[1]!.start); // 110
  // Re-link pulls start[1] back to end[0].
  P.link(st, 0);
  assert.equal(st.linked[0], true);
  assert.equal(st.tracks[1]!.start, st.tracks[0]!.end);
});

test("handlesFor: linked boundaries yield one handle, detached yield two", () => {
  const st = P.initState(sampleResp()); // [linked, linked]
  assert.ok(st);
  // head + 2 boundaries (1 each) + tail = 4
  assert.equal(P.handlesFor(st).length, 4);
  P.detach(st, 0);
  // head + (2 handles for boundary 0) + (1 for boundary 1) + tail = 5
  assert.equal(P.handlesFor(st).length, 5);
});

test("validate flags short segments, overlaps, and out-of-bounds ends", () => {
  const st = P.initState(sampleResp());
  assert.ok(st);
  assert.deepEqual(P.validate(st), []);

  const s2 = P.initState(sampleResp());
  assert.ok(s2);
  s2.tracks[0]!.end = 0.5; // <1s
  assert.ok(P.validate(s2).some((e) => /shorter/.test(e)));

  const overlap = P.initState(sampleResp());
  assert.ok(overlap);
  overlap.linked[0] = false;
  overlap.tracks[0]!.end = 150;
  overlap.tracks[1]!.start = 100;
  assert.ok(P.validate(overlap).some((e) => /overlaps/.test(e)));

  const beyond = P.initState(sampleResp());
  assert.ok(beyond);
  beyond.tracks[2]!.end = 999;
  assert.ok(P.validate(beyond).some((e) => /past media duration/.test(e)));
});

test("buildPayload emits title/start_time/end_time rounded to 3dp", () => {
  const st = P.initState(sampleResp());
  assert.ok(st);
  st.tracks[0]!.end = 100.123456;
  st.tracks[1]!.start = 100.123456;
  const payload = P.buildPayload(st);
  assert.equal(payload.songs.length, 3);
  assert.deepEqual(Object.keys(payload.songs[0]!).sort(), ["end_time", "start_time", "title"]);
  assert.equal(payload.songs[0]!.end_time, 100.123);
});

test("single-track concert: only head/tail handles, mutually clamped", () => {
  const st = P.initState({
    set_list: ["Solo"],
    auto: [{ title: "Solo", start_time: 0, end_time: 200, duration: 200 }],
    user: null,
    media_duration: 250,
  });
  assert.ok(st);
  assert.equal(P.handlesFor(st).length, 2); // head + tail
  P.setEnd(st, 0, 999);
  assert.equal(st.tracks[0]!.end, 250); // clamped to duration
  P.setStart(st, 0, 999);
  assert.ok(close(st.tracks[0]!.start, st.tracks[0]!.end - P.MIN_SEG));
});
