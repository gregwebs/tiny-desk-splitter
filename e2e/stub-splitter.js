#!/usr/bin/env node
"use strict";

// Stand-in for live-set-splitter in e2e runs (passed via --splitter-bin): a
// real executable the server spawns, not a mock. Takes the same CLI as the
// real splitter — `<config.json> --input-file <file> --output-dir <dir>` —
// reads the set list from the config, and "splits" by copying the input file
// to one playable file per song. Copying the fixture's wav keeps every
// produced track genuinely decodable by Chromium.

const fs = require("fs");
const path = require("path");

// Mirrors concert-tracker's sanitize_filename (model.rs) so the server's
// find_track_file locates what we write.
function sanitizeFilename(input) {
  let s = input.replace(/[/\\:*?"<>|\0]/g, "_").replace(/__/g, "_");
  s = s.trim().replace(/^\.+|\.+$/g, "");
  return s.length ? s : "untitled";
}

function arg(flag) {
  const i = process.argv.indexOf(flag);
  if (i === -1 || i + 1 >= process.argv.length) {
    console.error(`stub-splitter: missing ${flag}`);
    process.exit(2);
  }
  return process.argv[i + 1];
}

const configPath = process.argv[2];
const inputFile = arg("--input-file");
const outputDir = arg("--output-dir");

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
}, DELAY_MS);
