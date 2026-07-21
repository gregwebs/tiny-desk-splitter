//! Silence-based recovery for songs that text-overlay detection missed.

use crate::audio;
use crate::concert_split::{AudioSegment, ConcertSplitProgress, SongSegment};
use concert_types::Song;

/// Status of each expected song after recovery, in set-list order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RecoveryResult {
    /// Song was already in `segments` before recovery ran.
    AlreadyFound,
    /// Song was missing but a boundary was inserted (audio silence or equal-split).
    Recovered,
    /// Song is still missing (no anchor pair, or the gap couldn't be filled).
    StillMissing,
}

/// Compute the adaptive silence threshold used both for recovery and for the
/// later refinement pass — keeping them identical means the boundaries we
/// insert here are exactly the silences the refinement step would consider.
pub(crate) fn adaptive_silence_threshold(energy_profile: &[f64]) -> f64 {
    let mean_energy: f64 = energy_profile.iter().sum::<f64>() / energy_profile.len() as f64;
    let adaptive = mean_energy * 0.25;
    adaptive.clamp(audio::ENERGY_THRESHOLD * 0.1, audio::ENERGY_THRESHOLD)
}

/// Where a recovered boundary came from, in order of preference.
#[derive(Clone, Copy, PartialEq, Debug)]
enum RecoverySource {
    /// A frame where the artist overlay was detected but the title was unreadable
    /// (an "unmatched overlay cluster"). The most reliable signal: a real title card
    /// appeared here, so it is treated as an overlay estimate (gets the pullback).
    Overlay,
    /// The midpoint of an audio silence span inside the gap.
    Silence,
    /// No candidate available; the gap was equally divided.
    EqualSplit,
}

/// Fill still-empty (`None`) slots of `chosen` from `candidates`, assigning each
/// candidate to the empty slot whose `expected` position it is closest to (iterating
/// slots in order, matching the original silence-only behavior). Enforces
/// `MIN_SONG_GAP_SECONDS` spacing both against boundaries an earlier tier already
/// chose and between candidates picked here. `candidates` must already be filtered
/// for gap-endpoint spacing (see [`candidates_in_gap`]).
fn fill_slots_by_proximity(
    chosen: &mut [Option<(f64, RecoverySource)>],
    expected: &[f64],
    mut candidates: Vec<f64>,
    source: RecoverySource,
) {
    // Drop candidates too close to a boundary an earlier tier already chose.
    let prechosen: Vec<f64> = chosen.iter().filter_map(|c| c.map(|(t, _)| t)).collect();
    candidates.retain(|&m| {
        prechosen
            .iter()
            .all(|&p| (m - p).abs() >= audio::MIN_SONG_GAP_SECONDS)
    });

    for slot in 0..chosen.len() {
        if chosen[slot].is_some() {
            continue;
        }
        if candidates.is_empty() {
            break;
        }
        let exp = expected[slot];
        // Pick the candidate closest to this slot's expected position.
        let (best_i, &best) = candidates
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (*a - exp).abs().partial_cmp(&(*b - exp).abs()).unwrap())
            .unwrap();
        chosen[slot] = Some((best, source));
        candidates.remove(best_i);
        // Drop other candidates within the spacing window so a later slot can't
        // pick a near-duplicate.
        candidates.retain(|&m| (m - best).abs() >= audio::MIN_SONG_GAP_SECONDS);
    }
}

/// Candidates strictly inside the gap `(gap_start, gap_end)` that also clear the
/// `MIN_SONG_GAP_SECONDS` spacing from both endpoints.
pub(crate) fn candidates_in_gap(candidates: &[f64], gap_start: f64, gap_end: f64) -> Vec<f64> {
    candidates
        .iter()
        .copied()
        .filter(|&m| m > gap_start && m < gap_end)
        .filter(|&m| {
            (m - gap_start).abs() >= audio::MIN_SONG_GAP_SECONDS
                && (gap_end - m).abs() >= audio::MIN_SONG_GAP_SECONDS
        })
        .collect()
}

/// For each missing song that sits in an interior gap (between two found
/// boundaries), insert a `SongSegment` for it. Boundary candidates are taken, in
/// order of preference, from (1) `overlay_clusters` — frames where the artist
/// overlay was detected but the (short/stylized) title was unreadable, so the song
/// was dropped from text detection — then (2) the longest audio silences in the
/// gap, and finally (3) equal-spacing the gap when neither is available.
///
/// Overlay clusters are by far the most reliable signal (they mark where a real
/// title card appeared), so they win over silence within a gap. This is what
/// rescues e.g. yeule's 2-char "VV" title, whose overlay is detected but never
/// OCR-matched. See docs/change/2026-06-13-overlay-anchor-recovery.md.
///
/// Songs missing at the head (before the first found boundary) or tail (after
/// the last) are not recovered here — the head case is handled separately by
/// `first_song_missing_fallback`, and the tail case is out of scope.
///
/// Returns one `RecoveryResult` per song in `set_list` order so the caller can
/// build the still-missing list.
pub(crate) fn recover_missing_songs(
    segments: &mut Vec<SongSegment>,
    set_list: &[Song],
    overlay_clusters: &[f64],
    audio_data: &[f32],
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Vec<RecoveryResult> {
    let mut results: Vec<RecoveryResult> = set_list
        .iter()
        .map(|song| {
            if segments
                .iter()
                .any(|s| s.song.title.to_lowercase() == song.title.to_lowercase())
            {
                RecoveryResult::AlreadyFound
            } else {
                RecoveryResult::StillMissing
            }
        })
        .collect();

    // Compute silence spans once.
    let energy_profile = audio::calculate_energy_profile(audio_data);
    let threshold = adaptive_silence_threshold(&energy_profile);
    let silence_spans = audio::find_silence_spans(&energy_profile, threshold);
    let silence_midpoints: Vec<f64> = silence_spans.iter().map(|s| s.midpoint_seconds).collect();

    let mut i = 0;
    while i < set_list.len() {
        if results[i] != RecoveryResult::StillMissing {
            i += 1;
            continue;
        }

        // Find prev anchor (last AlreadyFound or Recovered before i).
        let prev_idx = (0..i)
            .rev()
            .find(|&j| results[j] != RecoveryResult::StillMissing);
        // Find run of missing songs starting at i.
        let mut run_end = i;
        while run_end + 1 < set_list.len() && results[run_end + 1] == RecoveryResult::StillMissing {
            run_end += 1;
        }
        // Find next anchor after run_end.
        let next_idx =
            ((run_end + 1)..set_list.len()).find(|&j| results[j] != RecoveryResult::StillMissing);

        let (prev_idx, next_idx) = match (prev_idx, next_idx) {
            (Some(p), Some(n)) => (p, n),
            // Head or tail run — leave these missing.
            _ => {
                i = run_end + 1;
                continue;
            }
        };

        let prev_segment = find_segment_for_song(segments, &set_list[prev_idx]);
        let next_segment = find_segment_for_song(segments, &set_list[next_idx]);
        let gap_start = prev_segment.segment.start_time;
        let gap_end = next_segment.segment.start_time;
        let missing_count = run_end - i + 1;
        let gap_size = gap_end - gap_start;

        // Expected (equal-length) position of each missing slot within the gap. Each
        // candidate tier picks, per slot, the candidate closest to this position —
        // more robust than "longest" when a gap holds both an end-of-song and a
        // start-of-song silence.
        let expected: Vec<f64> = (0..missing_count)
            .map(|slot| gap_start + ((slot + 1) as f64) * gap_size / ((missing_count + 1) as f64))
            .collect();

        // Two-tier candidate selection: prefer overlay clusters (a real title card
        // appeared there), then audio silences. Each tier fills only still-empty
        // slots and respects spacing against boundaries an earlier tier chose.
        let mut chosen: Vec<Option<(f64, RecoverySource)>> = vec![None; missing_count];
        fill_slots_by_proximity(
            &mut chosen,
            &expected,
            candidates_in_gap(overlay_clusters, gap_start, gap_end),
            RecoverySource::Overlay,
        );
        fill_slots_by_proximity(
            &mut chosen,
            &expected,
            candidates_in_gap(&silence_midpoints, gap_start, gap_end),
            RecoverySource::Silence,
        );

        let unfilled_count = chosen.iter().filter(|c| c.is_none()).count();
        if unfilled_count > 0 {
            let missing_titles: Vec<&str> =
                (i..=run_end).map(|j| set_list[j].title.as_str()).collect();
            progress(ConcertSplitProgress::Warning(format!(
                "overlay/silence recovery only filled {}/{} boundaries in gap {:.2}s–{:.2}s; equally spacing remaining songs: {:?}",
                missing_count - unfilled_count,
                missing_count,
                gap_start,
                gap_end,
                missing_titles
            )));
            // Build current anchors (gap endpoints + already-filled slots), sort,
            // then repeatedly bisect the widest subgap to fill unfilled slots.
            let mut anchors: Vec<f64> = Vec::with_capacity(2 + missing_count);
            anchors.push(gap_start);
            anchors.push(gap_end);
            for c in chosen.iter().flatten() {
                anchors.push(c.0);
            }
            anchors.sort_by(|a, b| a.partial_cmp(b).unwrap());

            for entry in chosen.iter_mut().take(missing_count) {
                if entry.is_some() {
                    continue;
                }
                let (widest_i, _) = anchors
                    .windows(2)
                    .enumerate()
                    .max_by(|(_, a), (_, b)| (a[1] - a[0]).partial_cmp(&(b[1] - b[0])).unwrap())
                    .unwrap();
                let mid = (anchors[widest_i] + anchors[widest_i + 1]) / 2.0;
                *entry = Some((mid, RecoverySource::EqualSplit));
                anchors.insert(widest_i + 1, mid);
            }
        }

        // Every slot is filled now; unwrap and order chronologically.
        let mut chosen: Vec<(f64, RecoverySource)> =
            chosen.into_iter().map(|c| c.unwrap()).collect();
        chosen.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        for (offset, &(start_time, source)) in chosen.iter().enumerate() {
            let song_idx = i + offset;
            let source_label = match source {
                RecoverySource::Overlay => "title overlay",
                RecoverySource::Silence => "audio silence",
                RecoverySource::EqualSplit => "equal-split",
            };
            progress(ConcertSplitProgress::Diagnostic(format!(
                "Recovered missing song '{}' at {:.2}s ({}, between '{}' and '{}')",
                set_list[song_idx].title,
                start_time,
                source_label,
                set_list[prev_idx].title,
                set_list[next_idx].title,
            )));
            segments.push(SongSegment {
                song: set_list[song_idx].clone(),
                segment: AudioSegment {
                    start_time,
                    end_time: gap_end,
                    is_song: true,
                },
                // An overlay-sourced boundary is an overlay estimate (~OVERLAY_DELAY
                // late), so it gets the same audio pullback as a detected overlay.
                // Silence and equal-split boundaries are not overlay estimates.
                start_from_overlay: source == RecoverySource::Overlay,
            });
            results[song_idx] = RecoveryResult::Recovered;
        }

        i = run_end + 1;
    }

    // Re-sort segments by start time so downstream code sees them in order.
    segments.sort_by(|a, b| {
        a.segment
            .start_time
            .partial_cmp(&b.segment.start_time)
            .unwrap()
    });

    // Tighten end_times so each song's end matches the next song's start.
    for i in 0..segments.len() {
        if i + 1 < segments.len() {
            segments[i].segment.end_time = segments[i + 1].segment.start_time;
        }
    }

    results
}

fn find_segment_for_song<'a>(segments: &'a [SongSegment], song: &Song) -> &'a SongSegment {
    segments
        .iter()
        .find(|s| s.song.title.to_lowercase() == song.title.to_lowercase())
        .expect("caller must guarantee the song is present")
}

#[cfg(test)]
mod tests_recover_missing_songs {
    use super::*;
    use crate::audio::frames_per_second;

    fn songs(titles: &[&str]) -> Vec<Song> {
        titles
            .iter()
            .map(|t| Song {
                title: t.to_string(),
            })
            .collect()
    }

    fn segment(title: &str, start: f64) -> SongSegment {
        SongSegment {
            song: Song {
                title: title.to_string(),
            },
            segment: AudioSegment {
                start_time: start,
                end_time: start,
                is_song: true,
            },
            start_from_overlay: false,
        }
    }

    /// Build a synthetic audio waveform that has loud sections interleaved with
    /// silent blocks. `blocks` is a list of (seconds, is_silent) tuples. Loud
    /// sections produce ±0.5 amplitude, silence produces ~0. The resulting
    /// waveform, when run through `calculate_energy_profile` and
    /// `find_silence_spans`, surfaces a span at the right time.
    fn synth_audio(blocks: &[(f64, bool)]) -> Vec<f32> {
        let sr = audio::SAMPLE_RATE as f64;
        let mut samples = Vec::new();
        let mut t: f64 = 0.0;
        for &(seconds, is_silent) in blocks {
            let count = (seconds * sr) as usize;
            for i in 0..count {
                if is_silent {
                    samples.push(0.0);
                } else {
                    // Audible sine-like wave at 100Hz so RMS is well above the
                    // threshold of 0.005.
                    let phase = (t + i as f64 / sr) * 2.0 * std::f64::consts::PI * 100.0;
                    samples.push(0.5 * phase.sin() as f32);
                }
            }
            t += seconds;
        }
        samples
    }

    #[test]
    fn k1_recovers_at_obvious_silence_midpoint() {
        // 60s gap with a single 5s silence centered at ~30s.
        let audio = synth_audio(&[
            (10.0, false), // song A body
            (20.0, false), // ...
            (5.0, true),   // silence between A and B (midpoint ~32.5s)
            (25.0, false), // song B body
        ]);
        let set_list = songs(&["A", "B"]);
        let mut segments = vec![segment("A", 0.0), segment("B", 60.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});

        // Both songs reported as already-found (we seeded both), so nothing to do.
        assert_eq!(results, vec![RecoveryResult::AlreadyFound; 2]);

        // Now drop B and put a missing song between them.
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 60.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        assert_eq!(
            results,
            vec![
                RecoveryResult::AlreadyFound,
                RecoveryResult::Recovered,
                RecoveryResult::AlreadyFound,
            ]
        );
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        // Silence centered around 32.5s. Allow generous slack for energy smoothing.
        assert!(
            (b.segment.start_time - 32.5).abs() < 3.0,
            "B placed at {:.2}s",
            b.segment.start_time
        );
    }

    #[test]
    fn k1_picks_silence_closest_to_expected_midpoint() {
        // Two silences in a 100s gap. Gap midpoint is 50s. A silence sits at
        // ~26.5s (closer to the "Carillon ended" side) and another at ~46s
        // (closer to the expected boundary). The shorter-but-better-positioned
        // silence at ~46s must win — picking the longest one regardless of
        // position would mis-attribute the end-of-prev-song pause as the start
        // of the missing song. This regression covers the Sean Shibe case
        // where the longest silence in the gap was right before the next
        // detected song.
        let audio = synth_audio(&[
            (25.0, false),
            (6.0, true), // longest silence; midpoint ~28s, far from midpoint=50
            (15.0, false),
            (4.0, true), // shorter; midpoint ~48s, close to midpoint=50
            (50.0, false),
        ]);
        // gap_start=0, gap_end=100, expected midpoint=50.
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 100.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        assert_eq!(results[1], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        assert!(
            (b.segment.start_time - 48.0).abs() < 4.0,
            "expected closest-to-midpoint pick near 48s, got {:.2}s",
            b.segment.start_time
        );
    }

    #[test]
    fn k2_chronological_ordering_of_chosen_silences() {
        // Three qualifying silences: 7s long at ~33s, 6s at ~62s, 5s at ~92s.
        let audio = synth_audio(&[
            (30.0, false),
            (7.0, true), // longest, mid ~33.5s
            (20.0, false),
            (6.0, true), // mid ~62.5s
            (20.0, false),
            (5.0, true), // mid ~92s
            (15.0, false),
        ]);
        let set_list = songs(&["A", "B", "C", "D"]);
        let mut segments = vec![segment("A", 0.0), segment("D", 105.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        assert_eq!(results[1], RecoveryResult::Recovered);
        assert_eq!(results[2], RecoveryResult::Recovered);

        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        let c = segments.iter().find(|s| s.song.title == "C").unwrap();
        // Two longest silences are 7s (~33.5s) and 6s (~62.5s); B should land at the earlier one.
        assert!(
            b.segment.start_time < c.segment.start_time,
            "expected B<C chronologically; got B={:.2} C={:.2}",
            b.segment.start_time,
            c.segment.start_time
        );
        assert!((b.segment.start_time - 33.5).abs() < 3.0, "{:?}", b.segment);
        assert!((c.segment.start_time - 62.5).abs() < 3.0, "{:?}", c.segment);
    }

    #[test]
    fn spacing_constraint_forces_equal_split_for_close_silences() {
        // Two silences only ~4s apart inside a 200s gap.
        let audio = synth_audio(&[
            (90.0, false),
            (3.0, true), // mid ~91.5s
            (1.0, false),
            (3.0, true), // mid ~96s (only ~4.5s after first)
            (103.0, false),
        ]);
        let set_list = songs(&["A", "B", "C", "D"]);
        let mut segments = vec![segment("A", 0.0), segment("D", 200.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        // Both B and C should be recovered, but C via equal-split since the
        // second silence is within MIN_SONG_GAP_SECONDS=20s of the first.
        assert_eq!(results[1], RecoveryResult::Recovered);
        assert_eq!(results[2], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        let c = segments.iter().find(|s| s.song.title == "C").unwrap();
        // The two recovered boundaries must be at least MIN_SONG_GAP_SECONDS apart.
        assert!(
            (c.segment.start_time - b.segment.start_time).abs() >= audio::MIN_SONG_GAP_SECONDS,
            "B={:.2} C={:.2}",
            b.segment.start_time,
            c.segment.start_time
        );
    }

    #[test]
    fn equal_split_fires_when_no_silence_qualifies() {
        // 60s of loud music, no silence at all.
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 60.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        assert_eq!(results[1], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        // Equal split between 0 and 60 puts B at 30.
        assert!((b.segment.start_time - 30.0).abs() < 0.001);
    }

    #[test]
    fn missing_at_head_is_not_recovered() {
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B"]);
        // B is found at 30s but A is missing — no anchor before A.
        let mut segments = vec![segment("B", 30.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        assert_eq!(results[0], RecoveryResult::StillMissing);
        assert_eq!(results[1], RecoveryResult::AlreadyFound);
        assert_eq!(
            segments.len(),
            1,
            "no segment should have been inserted for A"
        );
    }

    #[test]
    fn missing_at_tail_is_not_recovered() {
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B"]);
        let mut segments = vec![segment("A", 0.0)];
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        assert_eq!(results[0], RecoveryResult::AlreadyFound);
        assert_eq!(results[1], RecoveryResult::StillMissing);
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn all_found_is_noop() {
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B"]);
        let mut segments = vec![segment("A", 0.0), segment("B", 30.0)];
        let before = segments.clone();
        let results = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});
        assert_eq!(results, vec![RecoveryResult::AlreadyFound; 2]);
        assert_eq!(segments.len(), before.len());
        for (a, b) in segments.iter().zip(before.iter()) {
            assert!((a.segment.start_time - b.segment.start_time).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn end_times_chain_through_inserted_segments() {
        let audio = synth_audio(&[(10.0, false), (5.0, true), (15.0, false)]);
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 30.0)];
        let _ = recover_missing_songs(&mut segments, &set_list, &[], &audio, &mut |_| {});

        // After recovery, segments should be sorted by start_time and chained:
        // A.end == B.start, B.end == C.start.
        segments.sort_by(|a, b| {
            a.segment
                .start_time
                .partial_cmp(&b.segment.start_time)
                .unwrap()
        });
        let a = &segments[0];
        let b = &segments[1];
        let c = &segments[2];
        assert!((a.segment.end_time - b.segment.start_time).abs() < f64::EPSILON);
        assert!((b.segment.end_time - c.segment.start_time).abs() < f64::EPSILON);
    }

    /// Sanity check that the synthetic audio actually surfaces silence at the
    /// expected position via the real audio pipeline — guards against the
    /// energy-smoothing window swallowing short silences in fixture data.
    #[test]
    fn synth_audio_produces_detectable_silence() {
        let audio = synth_audio(&[(10.0, false), (5.0, true), (10.0, false)]);
        let profile = audio::calculate_energy_profile(&audio);
        let threshold = adaptive_silence_threshold(&profile);
        let spans = audio::find_silence_spans(&profile, threshold);
        assert!(!spans.is_empty(), "expected at least one silence span");
        let center = spans[0].midpoint_seconds;
        assert!(
            (center - 12.5).abs() < 2.0,
            "expected silence midpoint near 12.5s, got {:.2}s (fps={:.2})",
            center,
            frames_per_second()
        );
    }

    #[test]
    fn overlay_cluster_preferred_over_silence() {
        // Reproduces the yeule "VV" case in miniature: A and C are found, B's title
        // overlay was detected but unreadable (overlay cluster at 200s), and the gap
        // also contains an audio silence near 500s. The overlay cluster must win, so
        // B is anchored at ~200s — NOT mis-placed at the 500s silence.
        let audio = synth_audio(&[(498.0, false), (4.0, true), (98.0, false)]); // silence ~500s
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 600.0)];
        let results =
            recover_missing_songs(&mut segments, &set_list, &[200.0], &audio, &mut |_| {});
        assert_eq!(results[1], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        assert!(
            (b.segment.start_time - 200.0).abs() < 1.0,
            "B should anchor on the overlay cluster (200s), got {:.2}s",
            b.segment.start_time
        );
        // Overlay-sourced recovery is an overlay estimate, so it gets the pullback.
        assert!(
            b.start_from_overlay,
            "overlay-recovered song must be marked start_from_overlay"
        );
    }

    #[test]
    fn overlay_cluster_outside_gap_is_ignored() {
        // A cluster outside the missing song's gap (here after C) must not be used;
        // recovery falls back to the silence in the gap.
        let audio = synth_audio(&[(290.0, false), (4.0, true), (306.0, false)]); // silence ~292s
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 600.0)];
        // Cluster at 800s is past C — irrelevant to B's gap (0,600).
        let results =
            recover_missing_songs(&mut segments, &set_list, &[800.0], &audio, &mut |_| {});
        assert_eq!(results[1], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        assert!(
            (b.segment.start_time - 292.0).abs() < 3.0,
            "B should fall back to the in-gap silence (~292s), got {:.2}s",
            b.segment.start_time
        );
        assert!(
            !b.start_from_overlay,
            "silence-recovered song must not be marked start_from_overlay"
        );
    }

    #[test]
    fn candidates_in_gap_filters_endpoints_and_outside() {
        // MIN_SONG_GAP_SECONDS = 20. Inside (0,600): 5 is too close to start, 595 too
        // close to end, 700 is outside; only 300 survives.
        let got = candidates_in_gap(&[5.0, 300.0, 595.0, 700.0], 0.0, 600.0);
        assert_eq!(got, vec![300.0]);
    }
}
