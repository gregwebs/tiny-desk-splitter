#!/usr/bin/env node
"use strict";

// Stand-in for live-set-splitter in e2e runs (passed via --splitter-bin): a
// real executable the server spawns, not a mock. Takes the same CLI as the
// real splitter — `<config.json> --input-file <file> --output-dir <dir>` —
// reads the set list from the config, and "splits" by copying the input file
// to one playable file per song. Copying the fixture's wav keeps every
// produced track genuinely decodable by Chromium.
//
// Also handles --emit-interludes and --media-duration: when those flags are
// present (user-timestamps mode), derives gap spans from --timestamps-file and
// writes interlude_NN.m4a stubs. The tracker's find_interlude_file only probes
// .mp4/.m4a, so interludes always use .m4a regardless of the source extension.

const fs = require("fs");
const path = require("path");

// Mirrors concert-tracker's sanitize_filename (model.rs) so the server's
// find_track_file locates what we write.
function sanitizeFilename(input) {
  let s = input.replace(/[/\\:*?"<>|\0]/g, "_").replace(/__/g, "_");
  s = s.trim().replace(/^\.+|\.+$/g, "");
  return s.length ? s : "untitled";
}

function optArg(flag) {
  const i = process.argv.indexOf(flag);
  if (i === -1 || i + 1 >= process.argv.length) return null;
  return process.argv[i + 1];
}

function arg(flag) {
  const val = optArg(flag);
  if (val === null) {
    console.error(`stub-splitter: missing ${flag}`);
    process.exit(2);
  }
  return val;
}

const configPath = process.argv[2];
const inputFile = arg("--input-file");
const outputDir = arg("--output-dir");
const emitInterludes = process.argv.includes("--emit-interludes");
const mediaDurationStr = optArg("--media-duration");
const mediaDuration = mediaDurationStr !== null ? parseFloat(mediaDurationStr) : null;
const tsFilePath = optArg("--timestamps-file");

// Minimum gap span worth capturing (mirrors concert_types::MIN_INTERLUDE_SECONDS).
const MIN_INTERLUDE_SECONDS = 1.0;

// Mirrors concert_types::interlude_filename_stem.
function interludeFilenameStem(index) {
  return `interlude_${String(index).padStart(2, "0")}`;
}

// Derive uncovered spans from an ordered list of {start_time, end_time} songs
// and the total media duration. Returns [{index, start_time, end_time}, ...].
function deriveInterludes(songs, duration) {
  const interludes = [];
  let cursor = 0.0;
  for (const song of songs) {
    if (song.start_time - cursor >= MIN_INTERLUDE_SECONDS) {
      interludes.push({
        index: interludes.length + 1,
        start_time: cursor,
        end_time: song.start_time,
      });
    }
    cursor = song.end_time;
  }
  if (duration - cursor >= MIN_INTERLUDE_SECONDS) {
    interludes.push({
      index: interludes.length + 1,
      start_time: cursor,
      end_time: duration,
    });
  }
  return interludes;
}

// A short, deterministic "work" delay so specs can observe the in-flight
// splitting state (preparing mark, disabled buttons) before the files land.
const DELAY_MS = 1000;

setTimeout(() => {
  const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
  fs.mkdirSync(outputDir, { recursive: true });
  const ext = path.extname(inputFile).slice(1) || "wav";
  for (const song of config.set_list || []) {
    const dest = path.join(outputDir, `${sanitizeFilename(song.title)}.${ext}`);
    fs.copyFileSync(inputFile, dest);
    console.log(`stub-splitter: wrote ${dest}`);
  }

  if (emitInterludes && mediaDuration !== null && tsFilePath !== null) {
    // Purge stale interlude files before re-emitting (mirrors splitter behaviour).
    const stalePattern = /^interlude_\d{2}\.(mp4|m4a)$/;
    for (const name of fs.readdirSync(outputDir)) {
      if (stalePattern.test(name)) {
        fs.unlinkSync(path.join(outputDir, name));
        console.log(`stub-splitter: removed stale interlude ${name}`);
      }
    }

    // Read timestamps written by the tracker for this user split.
    const tsData = JSON.parse(fs.readFileSync(tsFilePath, "utf8"));
    const songs = tsData.songs || [];
    const interludes = deriveInterludes(songs, mediaDuration);

    // Write each interlude as .m4a so find_interlude_file (which only probes
    // .mp4/.m4a) can locate them. Content is a copy of the input wav — the
    // tracker only checks existence, not decodability.
    for (const il of interludes) {
      const stem = interludeFilenameStem(il.index);
      const dest = path.join(outputDir, `${stem}.m4a`);
      fs.copyFileSync(inputFile, dest);
      console.log(`stub-splitter: wrote interlude ${dest}`);
    }
  }
}, DELAY_MS);
