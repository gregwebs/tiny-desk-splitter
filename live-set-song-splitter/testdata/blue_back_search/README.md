# `blue_back_search` fixture

Frames captured from the back-search (start-time refinement) for the song **"Blue"**
in the *Bloc Party — Tiny Desk Concert* video, via `--analyze-images`.

These are the cropped overlay frames the refinement OCR'd and **correctly** matched
as the "Bloc Party / Blue" overlay (you can see the overlay text in `73.png`,
`74.png`, `75.png`). For each source frame `N.png`, the refinement also extracts a
black-and-white variant `Nbw.png` into the *same* directory and OCRs both.

## The bug this fixture pins

`frame_number_from_image_filename` parses `Nbw.png` to `0` (the `bw` suffix makes
the stem unparseable). The back-search listed every `*.png` and counted the result
with `frames.len()`, so each `Nbw.png` counted as an extra frame: for K source
frames it saw `2*K` and pushed the refined start ~K frames (~3s at 24fps) too early.

Concretely, "Blue"'s overlay (frame 73 of 75 source frames) was mapped to
**762.504344s** instead of ~765.6s, even though the overlay only appears on screen
at ~765s.

The fix excludes the `bw` variants from the refined-frame listing (`is_source_frame`,
mirroring the detection pass) so `frames.len()` is the source count, and maps the
matched frame back to a source-video frame via `refined_match_to_source_frame`. The
`bw` variants here are only kept as fixtures for the `is_source_frame` /
`refined_listing_filter_drops_bw_variants` tests in `src/main.rs` — the back-search
no longer extracts them.
