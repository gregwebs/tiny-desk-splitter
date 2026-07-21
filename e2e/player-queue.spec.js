const { test, expect, openTracks } = require("./fixtures");

// Drives the real player against the isolated fixture (no mocks). Fixture concerts:
//   1 "Audio Concert"  — Celular, Limbo, Track Three, Dando Vueltas   (wav, audio)
//   2 "Second Concert" — Song One, Song Two, Song Three               (wav, audio)
//   3 "Video Concert"  — Clip One(webm), Audio Song(wav), Clip Two(webm),
//                        Clip Three(webm), Raw Take(mkv, non-playable)
//   4 "Liked Concert"  — Liked Song (wav, liked in the DB)
//   5 "Deleted-First Concert" — Gone Opener (deleted, no file), Survivor One,
//                        Survivor Two (wav)
// Media is generated in Chromium-playable codecs (wav / VP8+Vorbis webm).
const AUDIO = 1;
const SECOND = 2;
const VIDEO = 3;
const LIKED = 4;
const DELETED_FIRST = 5;

function trackButton(page, concertId, trackIdx) {
  return page.locator(
    `[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`
  );
}

// Reveal the card's track list (hover on the listing; already visible on the
// detail page) and wait for the track buttons to exist.
async function expandTracks(page, concertId) {
  await openTracks(page, concertId);
  await page.waitForSelector(
    `[data-concert-id="${concertId}"][data-track-idx="0"]`
  );
}

async function waitForPlaying(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

async function focusPageBody(page) {
  await page.evaluate(() => {
    document.activeElement && document.activeElement.blur();
    document.body.tabIndex = -1;
    document.body.focus();
  });
}

// Expand a concert's tracks and start the given track playing. Waits for the
// player bar to actually render the track title before returning: waitForPlaying
// only observes the external <audio> element's `paused` flag, which flips before
// the Foldkit view re-renders #player-title — a caller that immediately
// interacts with player-bar controls (e.g. `.focus()`, which silently no-ops on
// a not-yet-visible element) can otherwise race that render. Likewise
// #player-seek stays `disabled` until its own separate loadedmetadata/
// timeupdate event lands (see model.ts's audioTime) — a caller that focuses
// it right after the title appears, before that arrives, hits the same
// silent-no-op-focus failure mode on a still-disabled control.
async function playTrack(page, concertId, trackIdx) {
  await expandTracks(page, concertId);
  await trackButton(page, concertId, trackIdx).click();
  await waitForPlaying(page);
  await expect(page.locator("#player-title")).not.toBeEmpty();
  await expect(page.locator("#player-seek")).toBeEnabled();
}

// Wait until the whole media file is buffered, so killing the server (to force a
// real network failure) doesn't starve the element and stop playback.
async function waitForFullyBuffered(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return (
      a &&
      a.duration > 0 &&
      a.buffered.length > 0 &&
      a.buffered.end(a.buffered.length - 1) >= a.duration - 0.2
    );
  });
}

async function simulateTrackEnd(page) {
  await page.evaluate(() => {
    const a = document.getElementById("player-audio");
    a.pause();
    a.dispatchEvent(new Event("ended"));
  });
}

// The track-list like (☆/★) and delete (trash) buttons live in the same <li> as
// the track's listen button.
function trackListLikeButton(page, concertId, trackIdx) {
  return page
    .locator("li")
    .filter({ has: trackButton(page, concertId, trackIdx) })
    .locator(".btn-like");
}
function trackListDeleteButton(page, concertId, trackIdx) {
  return page
    .locator("li")
    .filter({ has: trackButton(page, concertId, trackIdx) })
    .locator(".btn-delete");
}

test.describe("Player Queue", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("clicking a track with nothing playing starts playback immediately", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);

    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Celular");
    await expect(page.locator("#player-track")).toHaveText("#1");
    await expect(page.locator("#player-track")).toBeVisible();
    await expect(page.locator("#player-queue-badge")).toBeHidden();
  });

  test("the card's tracks button plays the split tracks", async ({
    page,
  }) => {
    // Hover first so the card's hover layout swap settles before the click.
    // Use evaluate to bypass pointer-position interactions that can cause the
    // hover state (and thus button visibility) to be lost in --single-process.
    await openTracks(page, AUDIO);
    await page
      .locator(`#concert-${AUDIO} .card-tracks-row button.btn-tracks`)
      .evaluate(el => el.click());

    await waitForPlaying(page);
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Celular");
    await expect(page.locator("#player-track")).toHaveText("#1");
  });

  test("the tracks button skips a deleted first track and plays the next", async ({
    page,
  }) => {
    // Deleted-First Concert's track 0 ("Gone Opener") has no file on disk, so
    // tracks/0/media-info 404s. Play must start the first surviving track
    // ("Survivor One", #2) rather than show "Error".
    await openTracks(page, DELETED_FIRST);
    await page
      .locator(`#concert-${DELETED_FIRST} .card-tracks-row button.btn-tracks`)
      .evaluate(el => el.click());

    await waitForPlaying(page);
    await expect(page.locator("#player-title")).toHaveText("Survivor One");
    await expect(page.locator("#player-track")).toHaveText("#2");
  });

  test("Next button is disabled when nothing is next to play", async ({
    page,
  }) => {
    // Last track in the concert: no following track to advance to.
    await playTrack(page, AUDIO, 3);

    await expect(page.locator("#player-next")).toBeDisabled();

    // The guard must also stop the public skip API from pausing the track.
    await page.evaluate(() => Player.skipToNext());
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Next button re-enables once a track is queued", async ({ page }) => {
    await playTrack(page, AUDIO, 3); // last track, nothing next
    await expect(page.locator("#player-next")).toBeDisabled();
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    // Queue a track from another concert — Next now has somewhere to go.
    // Use evaluate to avoid real pointer events being intercepted by the fixed
    // player bar on top of the card list.
    await expandTracks(page, SECOND);
    await trackButton(page, SECOND, 0).evaluate(el => el.click());
    await expect(page.locator("#player-next")).toBeEnabled();
  });

  test("Back button is disabled on the first track", async ({ page }) => {
    await playTrack(page, AUDIO, 0);

    await expect(page.locator("#player-prev")).toBeDisabled();

    await page.evaluate(() => Player.skipToPrev());
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Back button plays the previous track", async ({ page }) => {
    await playTrack(page, AUDIO, 1); // Limbo — a track behind us
    await expect(page.locator("#player-title")).toHaveText("Limbo");
    await expect(page.locator("#player-prev")).toBeEnabled();

    await page.locator("#player-prev").click();

    await expect(page.locator("#player-title")).toHaveText("Celular");
    await expect(page.locator("#player-prev")).toBeDisabled();
    await waitForPlaying(page);
  });

  test("clicking a track while playing enqueues it", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expandTracks(page, SECOND);
    await trackButton(page, SECOND, 0).evaluate(el => el.click());

    await expect(page.locator("#player-queue-badge")).toBeVisible();
    await expect(page.locator("#player-queue-badge")).toHaveText("1");
    await expect(page.locator("#player-title")).toHaveText("Celular");
  });

  test("clicking currently-playing track toggles pause", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    // No loop=true here: we're intentionally toggling pause/play, not letting
    // the track run; loop would not interfere but is unnecessary.

    // Use evaluate to avoid real pointer events being intercepted by the fixed
    // player bar on top of the card list.
    await trackButton(page, AUDIO, 0).evaluate(el => el.click());
    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && a.paused;
    });

    await trackButton(page, AUDIO, 0).evaluate(el => el.click());
    await waitForPlaying(page);
  });

  test("next button plays from queue", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expandTracks(page, SECOND);
    await trackButton(page, SECOND, 0).evaluate(el => el.click());
    await expect(page.locator("#player-queue-badge")).toHaveText("1");

    await page.locator("#player-next").click();

    await expect(page.locator("#player-title")).toHaveText("Song One");
    await expect(page.locator("#player-queue-badge")).toBeHidden();
  });

  test("next button auto-advances in concert when queue is empty", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);

    await page.locator("#player-next").click();

    await expect(page.locator("#player-title")).toHaveText("Limbo");
  });

  test("track ending with nothing next stops cleanly without an error banner", async ({
    page,
  }) => {
    // Regression: the next-media-info fetch 404s (its documented "no later
    // playable track" signal) when the last track ends with an empty queue —
    // that used to be misread as a failure and show "Couldn't load next
    // track" even though reaching the end of the set list is normal.
    await playTrack(page, AUDIO, 3); // last track, nothing after it
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await simulateTrackEnd(page);

    await expect(page.locator("#player-error")).toBeHidden();
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
  });

  test("when track ends, queued song plays instead of auto-advance", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    // loop=true prevents native track-end from firing before simulateTrackEnd;
    // the synthetic "ended" event dispatched by simulateTrackEnd still reaches
    // the player handler regardless of loop state.
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expandTracks(page, SECOND);
    await trackButton(page, SECOND, 1).evaluate(el => el.click());
    await expect(page.locator("#player-queue-badge")).toHaveText("1");

    await simulateTrackEnd(page);

    await expect(page.locator("#player-title")).toHaveText("Song Two", {
      timeout: 5000,
    });
    await expect(page.locator("#player-queue-badge")).toBeHidden();
  });

  test("after queue drains, auto-advance follows last queued concert", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expandTracks(page, SECOND);
    await trackButton(page, SECOND, 1).evaluate(el => el.click()); // queue Song Two

    await page.locator("#player-next").click();
    await expect(page.locator("#player-title")).toHaveText("Song Two");

    // Queue empty: auto-advance within the last-played concert (Second Concert).
    await page.locator("#player-next").click();
    await expect(page.locator("#player-title")).toHaveText("Song Three");
  });

  test("queue badge tooltip shows queued track titles", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    await expandTracks(page, SECOND);
    await trackButton(page, SECOND, 0).evaluate(el => el.click());
    await trackButton(page, SECOND, 1).evaluate(el => el.click());

    const badge = page.locator("#player-queue-badge");
    await expect(badge).toHaveText("2");
    const title = await badge.getAttribute("title");
    expect(title).toBeTruthy();
    expect(title.split("\n")).toHaveLength(2);
  });
});

test.describe("Player keyboard shortcuts", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("body-focused Space pauses an audio track and updates the play button", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await focusPageBody(page);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("#player-play-pause")).toHaveText("▶");
  });

  test("body-focused Space pauses inline video without folding the panel", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await focusPageBody(page);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("body-focused Space resumes paused media", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await focusPageBody(page);
    await page.keyboard.press("Space");
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
    await expect(page.locator("#player-play-pause")).toHaveText("⏸");
  });

  test("Space pauses after the Watch button opens inline video", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await expect(page.locator("#player-play-pause")).toHaveText("▶");
  });

  test("video-focused Space toggles playback", async ({ page }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await page.locator("#player-audio").focus();

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Space in an interactive control does not trigger the global pause shortcut", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.locator("#player-seek").focus();

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Space in contenteditable text does not trigger the global pause shortcut", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => {
      const editor = document.createElement("div");
      editor.id = "e2e-contenteditable";
      editor.contentEditable = "true";
      editor.textContent = "notes";
      document.getElementById("content").appendChild(editor);
      editor.focus();
    });

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("modified Space does not trigger the global pause shortcut", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await focusPageBody(page);

    await page.keyboard.press("Shift+Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("repeated Space keydown does not toggle playback again", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await focusPageBody(page);

    const prevented = await page.evaluate(async () => {
      const event = new KeyboardEvent("keydown", {
        bubbles: true,
        cancelable: true,
        code: "Space",
        key: " ",
        repeat: true,
      });
      document.dispatchEvent(event);
      // The player's keydown handling runs through an Effect Stream (queue →
      // async pull → preventDefault), so defaultPrevented isn't observable in
      // the same synchronous tick as dispatchEvent() the way a plain
      // addEventListener callback would be. A macrotask is enough for the
      // Stream to drain past the microtasks it schedules internally.
      await new Promise((r) => setTimeout(r, 0));
      return event.defaultPrevented;
    });

    expect(prevented).toBe(true);
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("body-focused Space before media is loaded does not activate the player", async ({
    page,
  }) => {
    await focusPageBody(page);

    await page.keyboard.press("Space");

    await expect(page.locator("#player-bar")).not.toHaveClass(/active/);
  });

  test("Escape folds an open video panel while playback continues", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await focusPageBody(page);

    await page.keyboard.press("Escape");

    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Escape from a control inside the player still folds the panel", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await page.locator("#player-watch").focus();

    await page.keyboard.press("Escape");

    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
  });

  test("Escape with the panel closed is a no-op", async ({ page }) => {
    await playTrack(page, VIDEO, 0);
    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
    await focusPageBody(page);

    await page.keyboard.press("Escape");

    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Escape in contenteditable text does not fold the panel", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await page.evaluate(() => {
      const editor = document.createElement("div");
      editor.id = "e2e-contenteditable";
      editor.contentEditable = "true";
      editor.textContent = "notes";
      document.getElementById("content").appendChild(editor);
      editor.focus();
    });

    await page.keyboard.press("Escape");

    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("modified Escape does not fold the panel", async ({ page }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await focusPageBody(page);

    await page.keyboard.press("Shift+Escape");

    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("Space after clicking queue toggle pauses playback without re-toggling the sidebar", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.locator("#player-queue-toggle").click();
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);
    await expect(page.locator("#player-play-pause")).toHaveText("▶");
  });

  test("Space after clicking queue toggle (sidebar closed) pauses without re-opening sidebar", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.locator("#player-queue-toggle").click();
    await expect(page.locator("body")).toHaveClass(/sidebar-open/);
    await page.locator("#player-queue-toggle").click();
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);
  });

  test("Space with focus on player-title pauses without toggling the sidebar", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.locator("#player-title").focus();
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);
  });

  test("Enter on focused player-title still toggles the sidebar", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.locator("#player-title").focus();
    await expect(page.locator("body")).not.toHaveClass(/sidebar-open/);

    await page.keyboard.press("Enter");

    await expect(page.locator("body")).toHaveClass(/sidebar-open/);
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Space on focused play-pause button toggles exactly once", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.locator("#player-play-pause").focus();

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("#player-play-pause")).toHaveText("▶");
  });

  test("Space on focused video-close button pauses without re-opening the video panel", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
    await page.locator("#player-video-close").focus();

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("Space on focused Next button pauses without skipping again", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await expect(page.locator("#player-next")).not.toBeDisabled();
    await page.locator("#player-next").click();
    await waitForPlaying(page);
    const titleAfterSkip = await page.locator("#player-title").textContent();

    await page.keyboard.press("Space");

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
    await expect(page.locator("#player-title")).toHaveText(titleAfterSkip);
  });
});

test.describe("Player host pause command", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("two rapid host toggles pause then resume from live media state", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);

    await page.evaluate(() => {
      const audio = document.getElementById("player-audio");
      audio.loop = true;
      const pause = audio.pause.bind(audio);
      const play = audio.play.bind(audio);
      window.__toggleTransitions = [];
      audio.pause = () => {
        window.__toggleTransitions.push("pause");
        pause();
      };
      audio.play = () => {
        window.__toggleTransitions.push("play");
        return play();
      };

      window.Player.togglePause();
      window.Player.togglePause();
    });

    await expect
      .poll(() => page.evaluate(() => window.__toggleTransitions))
      .toEqual(["pause", "play"]);
    await expect
      .poll(() =>
        page.evaluate(() => document.getElementById("player-audio").paused)
      )
      .toBe(false);
  });
});

test.describe("Inline video", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("Watch and Open buttons are hidden for audio-only tracks", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0); // audio track

    await expect(page.locator("#player-watch")).toBeHidden();
    await expect(page.locator("#player-open")).toBeHidden();
  });

  test("Watch and Open buttons are shown for video tracks", async ({ page }) => {
    await playTrack(page, VIDEO, 0); // Clip One (webm)

    await expect(page.locator("#player-watch")).toBeVisible();
    await expect(page.locator("#player-open")).toBeVisible();
  });

  test("Watch button folds the video panel open and closed", async ({ page }) => {
    await playTrack(page, VIDEO, 0);
    const panel = page.locator("#player-video-panel");
    await expect(panel).not.toHaveClass(/open/);

    await page.locator("#player-watch").click();
    await expect(panel).toHaveClass(/open/);
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);

    await page.locator("#player-watch").click();
    await expect(panel).not.toHaveClass(/open/);
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("toggling the panel does not reset playback position", async ({ page }) => {
    await playTrack(page, VIDEO, 0);
    await page.waitForFunction(
      () => document.getElementById("player-audio").currentTime > 0.1
    );

    await page.locator("#player-watch").click(); // open
    await page.locator("#player-watch").click(); // close

    const t = await page.evaluate(
      () => document.getElementById("player-audio").currentTime
    );
    expect(t).toBeGreaterThan(0);
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("Open button posts to the watch endpoint", async ({ page }) => {
    const watched = [];
    page.on("request", (r) => {
      if (
        r.method() === "POST" &&
        /\/concerts\/3\/tracks\/0\/watch$/.test(r.url())
      ) {
        watched.push(r.url());
      }
    });

    await playTrack(page, VIDEO, 0);
    await page.locator("#player-open").click();

    await expect.poll(() => watched.length).toBeGreaterThan(0);
  });

  test("Open pauses the JS player so audio doesn't double up", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-open").click();

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
  });

  test("Open is a no-op when nothing is playing", async ({ page }) => {
    await page.evaluate(() => Player.openExternal());
    await expect(page.locator("#player-bar")).not.toHaveClass(/active/);
  });

  test("watchTrackDirect starts inline playback and opens the panel", async ({
    page,
  }) => {
    await page.evaluate(() =>
      Player.watchTrackDirect(document.createElement("button"), 3, 0)
    );
    await waitForPlaying(page);

    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("inline Watch of a non-playable file does not open the panel", async ({
    page,
  }) => {
    // Raw Take is a .mkv: found on disk but not browser-playable, so the player
    // hands off to the system (window.open) and never opens the inline panel.
    await page.evaluate(() => {
      window.__opened = null;
      window.open = (u) => {
        window.__opened = u;
      };
    });

    await page.evaluate(() =>
      Player.watchTrackDirect(document.createElement("button"), 3, 4)
    );

    await expect
      .poll(() => page.evaluate(() => window.__opened))
      .toBe("/concert-files/Video Concert/Raw Take.mkv");
    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
  });

  test("auto-advancing from video to audio collapses the panel", async ({
    page,
  }) => {
    // Clip One (video, idx 0) → Audio Song (audio, idx 1).
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await simulateTrackEnd(page);

    await expect(page.locator("#player-title")).toHaveText("Audio Song");
    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
    await expect(page.locator("#player-watch")).toBeHidden();
  });

  test("auto-advancing from video to video keeps the panel open", async ({
    page,
  }) => {
    // Clip Two (video, idx 2) → Clip Three (video, idx 3).
    await playTrack(page, VIDEO, 2);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await simulateTrackEnd(page);

    await expect(page.locator("#player-title")).toHaveText("Clip Three");
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("ending the last track with no next collapses the video panel", async ({
    page,
  }) => {
    // Clip Three (idx 3) is the last playable track — the following Raw Take is
    // a non-playable .mkv, so there is nothing to advance to.
    await playTrack(page, VIDEO, 3);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await simulateTrackEnd(page);

    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
  });

  test("video panel stays open across Back/Forward navigation", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await page.locator('header a[href="/settings"]').click();
    await page.waitForFunction(() => location.pathname === "/settings");
    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");

    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  // The two dismiss-logic tests below run on looped *audio* playback with the
  // panel opened via Player.watch(): they test the outside-click handler's
  // dismiss/exempt rules, which are independent of what's decoding. Driving
  // them with real video (or a finite track) made them flaky — under sandbox
  // load the track ends (or the VP8 decode stalls) mid-test, auto-advance
  // plays the next audio track, and play() legitimately folds the panel.
  // Looping the element makes `ended` impossible while the test runs.
  const loopPlayback = (page) =>
    page.evaluate(() => {
      document.getElementById("player-audio").loop = true;
    });

  test("clicking dead space outside the player folds the video (audio keeps playing)", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await loopPlayback(page);
    await page.evaluate(() => Player.watch());
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    // A click on the empty page background above the panel dismisses the video, like
    // clicking Watch. (Dispatched on #content, the same pattern simulateTrackEnd uses.)
    await page.evaluate(() =>
      document
        .getElementById("content")
        .dispatchEvent(new MouseEvent("click", { bubbles: true }))
    );

    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("clicking an interactive control outside the player does not fold the video", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await loopPlayback(page);
    await page.evaluate(() => Player.watch());
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    // Clicking another concert's tracks button is a real control click: it
    // does its action (enqueues, since media is playing) and leaves the video
    // open (only dead-space clicks dismiss). Dispatched programmatically —
    // mirroring the dead-space test above — because the click event bubbling
    // to the document handler is what's under test, and driving the pointer
    // across cards (hover fetch + DOM injection mid-move) can crash the
    // sandbox's --single-process Chromium.
    await page
      .locator(`#concert-${SECOND} .card-tracks-row button.btn-tracks`)
      .evaluate((el) => el.click());
    await expect(page.locator("#player-queue-badge")).toBeVisible();

    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("the minimize button (revealed on mouse movement) folds the video", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    const panel = page.locator("#player-video-panel");
    await expect(panel).toHaveClass(/open/);

    // Hidden while the pointer is idle; mouse movement over the panel reveals it.
    await expect(panel).not.toHaveClass(/controls-visible/);
    await panel.dispatchEvent("mousemove");
    await expect(panel).toHaveClass(/controls-visible/);

    await page.locator("#player-video-close").click();
    await expect(panel).not.toHaveClass(/open/);
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("the minimize button fades back out once the pointer goes idle", async ({
    page,
  }) => {
    await playTrack(page, VIDEO, 0);
    await page.locator("#player-watch").click();
    const panel = page.locator("#player-video-panel");

    await panel.dispatchEvent("mousemove");
    await expect(panel).toHaveClass(/controls-visible/);
    await expect(panel).not.toHaveClass(/controls-visible/, { timeout: 4000 });
  });

  test("opening the panel from a Watch control outside the player keeps it open", async ({
    page,
  }) => {
    // The track-list Watch button (tracks.html) lives outside #player-container and opens
    // the panel via watchTrackDirect. Its own click must not be treated as an outside
    // dismiss — the dismiss listener attaches deferred, after the opening click. Mirror
    // that button here with a real, clickable element (the fixture's tracks don't render
    // the inline Watch button, which is gated on a template is_video flag).
    await page.evaluate(() => {
      const b = document.createElement("button");
      b.id = "e2e-watch-direct";
      b.textContent = "Watch";
      b.setAttribute("onclick", "Player.watchTrackDirect(this, 3, 0)");
      document.getElementById("content").appendChild(b);
    });

    await page.locator("#e2e-watch-direct").click();
    await waitForPlaying(page);

    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });
});

test.describe("Player like star", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("star is shown and reflects an unliked track", async ({ page }) => {
    await playTrack(page, AUDIO, 0); // not liked in the fixture

    const star = page.locator("#player-like");
    await expect(star).toBeVisible();
    await expect(star).toHaveText("☆");
    await expect(star).not.toHaveClass(/liked/);
  });

  test("star reflects a liked track", async ({ page }) => {
    await playTrack(page, LIKED, 0); // Liked Song is liked in the fixture

    const star = page.locator("#player-like");
    await expect(star).toBeVisible();
    await expect(star).toHaveText("★");
    await expect(star).toHaveClass(/liked/);
  });

  test("star is hidden during whole-album playback", async ({ page }) => {
    await page.evaluate(() =>
      Player.playAlbum(document.createElement("button"), 1)
    );
    await waitForPlaying(page);

    await expect(page.locator("#player-like")).toBeHidden();
  });

  test("clicking the star POSTs to /like and flips the star and the track-list button", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    const star = page.locator("#player-like");
    await expect(star).toHaveText("☆");

    await star.click();

    await expect(star).toHaveText("★");
    await expect(star).toHaveClass(/liked/);
    // The on-page track-list button mirrors the new state in place.
    await expect(trackListLikeButton(page, AUDIO, 0)).toHaveClass(/liked/);
  });

  test("a failing /like POST reverts the star", async ({ page, killServer }) => {
    await playTrack(page, AUDIO, 0);
    const star = page.locator("#player-like");
    await expect(star).toHaveText("☆");

    // Real failure: kill the server so the like POST network-errors. Buffer the
    // media first so playback survives the kill.
    await waitForFullyBuffered(page);
    await killServer();
    await star.click();

    // Optimistic flip is rolled back once the POST fails.
    await expect(star).toHaveText("☆");
    await expect(star).not.toHaveClass(/liked/);
    await expect(trackListLikeButton(page, AUDIO, 0)).not.toHaveClass(/liked/);
  });

  test("toggling the track-list star updates the player star (reverse sync)", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });

    // Use evaluate to avoid real pointer events being intercepted by the fixed
    // player bar on top of the card list.
    await trackListLikeButton(page, AUDIO, 0).evaluate(el => el.click());
    await expect
      .poll(async () => {
        const listLiked = await trackListLikeButton(page, AUDIO, 0).evaluate((el) =>
          el.classList.contains("liked")
        );
        const starLiked = await page
          .locator("#player-like")
          .evaluate((el) => el.classList.contains("liked"));
        return listLiked === starLiked;
      })
      .toBe(true);
  });

  test("toggling a different concert's star does not change the playing track's like", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    await page.evaluate(() => { document.getElementById("player-audio").loop = true; });
    const playing = trackListLikeButton(page, AUDIO, 0);
    const playingBefore = await playing.evaluate((el) =>
      el.classList.contains("liked")
    );

    // Toggle the like on an unrelated concert's track (real endpoint + swap).
    // Use evaluate to avoid real pointer events being intercepted by the fixed
    // player bar on top of the card list.
    await expandTracks(page, SECOND);
    await trackListLikeButton(page, SECOND, 0).evaluate(el => el.click());
    await expect(trackListLikeButton(page, SECOND, 0)).toBeVisible();

    expect(await playing.evaluate((el) => el.classList.contains("liked"))).toBe(
      playingBefore
    );
    await expect
      .poll(() =>
        page.locator("#player-like").evaluate((el) => el.classList.contains("liked"))
      )
      .toBe(playingBefore);
  });
});

test.describe("Player delete", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("delete button is shown when a track is playing", async ({ page }) => {
    await playTrack(page, AUDIO, 0);
    await expect(page.locator("#player-delete")).toBeVisible();
  });

  test("delete button is hidden during whole-album playback", async ({
    page,
  }) => {
    await page.evaluate(() =>
      Player.playAlbum(document.createElement("button"), 1)
    );
    await waitForPlaying(page);

    await expect(page.locator("#player-delete")).toBeHidden();
  });

  test("clicking delete posts to the delete endpoint and advances to the next track", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);

    await page.locator("#player-delete").click();

    await expect(page.locator("#player-title")).toHaveText("Limbo");
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
    // The deleted track is now unavailable on the page (but still clickable —
    // clicking it would trigger the automated re-split).
    await expect(trackButton(page, AUDIO, 0)).toHaveClass(
      /track-title-unavailable/
    );
  });

  test("deleting the last track stops playback", async ({ page }) => {
    await playTrack(page, AUDIO, 3); // last track, nothing after it

    await page.locator("#player-delete").click();

    await expect(page.locator("#player-bar")).not.toHaveClass(/active/);
    await expect(page.locator("#player-delete")).toBeHidden();
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
  });

  test("a failing delete keeps playback going and shows an error", async ({
    page,
    killServer,
  }) => {
    await playTrack(page, AUDIO, 0);

    // Real failure: kill the server so the delete POST network-errors. Buffer the
    // media first so playback keeps going after the kill.
    await waitForFullyBuffered(page);
    await killServer();
    await page.locator("#player-delete").click();

    await expect(page.locator("#player-error")).toBeVisible();
    await expect(page.locator("#player-title")).toHaveText("Celular");
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("the on-page track list is refreshed when a track is deleted", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);

    await page.locator("#player-delete").click();

    await expect(trackButton(page, AUDIO, 0)).toHaveClass(
      /track-title-unavailable/
    );
    // The card swap refreshes the tracks-button count and keeps the list.
    await expect(
      page.locator(`#concert-${AUDIO} .card-tracks-row button.btn-tracks`)
    ).toHaveText("tracks (3/4)");
    await expect(
      page.locator(`#concert-${AUDIO} ol.track-list li`)
    ).toHaveCount(4);
  });

  test("player-bar delete on the detail page updates the card's count", async ({
    page,
  }) => {
    // On the detail page the card's track list is always visible; playback
    // starts from it. The delete must refresh the card's count and keep the
    // list shown, with the deleted track now unavailable.
    await page.goto(`/concerts/${AUDIO}`);
    await trackButton(page, AUDIO, 0).click();
    await waitForPlaying(page);

    const tracksBtn = page.locator(
      `#concert-${AUDIO} .card-tracks-row button.btn-tracks`
    );
    await expect(tracksBtn).toHaveText("tracks (4)");

    await page.locator("#player-delete").click();

    await expect(tracksBtn).toHaveText("tracks (3/4)");
    await expect(
      page.locator(`#concert-${AUDIO} ol.track-list li`)
    ).toHaveCount(4);
    await expect(
      page.locator(`#concert-${AUDIO} ol.track-list .track-title-unavailable`)
    ).toHaveCount(1);
  });
});

test.describe("Starred tracks hide the delete button", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("player: starring the playing track hides #player-delete, unstarring restores it", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    const del = page.locator("#player-delete");
    await expect(del).toBeVisible();

    await page.locator("#player-like").click(); // star
    await expect(page.locator("#player-like")).toHaveText("★");
    await expect(del).toBeHidden();

    await page.locator("#player-like").click(); // unstar
    await expect(page.locator("#player-like")).toHaveText("☆");
    await expect(del).toBeVisible();
  });

  test("player: an already-starred track shows no delete button from the start", async ({
    page,
  }) => {
    await playTrack(page, LIKED, 0); // liked in the fixture

    await expect(page.locator("#player-like")).toHaveText("★");
    await expect(page.locator("#player-delete")).toBeHidden();
  });

  test("track list: starring the playing track from the player hides its row delete button", async ({
    page,
  }) => {
    await playTrack(page, AUDIO, 0);
    const rowDelete = trackListDeleteButton(page, AUDIO, 0);
    // The row lives in the hover-revealed tracks box, so assert the star/delete
    // relation via computed style (own display rule) rather than visibility,
    // which would be confounded by where the mouse happens to be.
    const rowDeleteDisplay = () =>
      rowDelete.evaluate((el) => getComputedStyle(el).display);

    await page.locator("#player-like").click();
    await expect.poll(rowDeleteDisplay).toBe("none");
    await expect(page.locator("#player-delete")).toBeHidden();

    await page.locator("#player-like").click();
    await expect.poll(rowDeleteDisplay).not.toBe("none");
    await expect(page.locator("#player-delete")).toBeVisible();
  });

  test("track list: a row's delete visibility tracks its own star (htmx button swap)", async ({
    page,
  }) => {
    await expandTracks(page, AUDIO);
    const relationHolds = async () => {
      const liked = await trackListLikeButton(page, AUDIO, 0).evaluate((el) =>
        el.classList.contains("liked")
      );
      const hidden = await trackListDeleteButton(page, AUDIO, 0).evaluate(
        (el) => getComputedStyle(el).display === "none"
      );
      return liked === hidden; // starred iff delete hidden
    };

    await trackListLikeButton(page, AUDIO, 0).click();
    await expect.poll(relationHolds).toBe(true);

    await trackListLikeButton(page, AUDIO, 0).click();
    await expect.poll(relationHolds).toBe(true);
  });
});

test.describe("Deleting a download/split keeps the player playing", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  // The player survived a card swap (no full reload): a sentinel set after load
  // persists, audio is still playing, currentTime keeps advancing, bar active.
  async function expectPlayerStillPlaying(page, t0) {
    expect(await page.evaluate(() => window.__noReload)).toBe(true);
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
    await expect
      .poll(() =>
        page.evaluate(() => document.getElementById("player-audio").currentTime)
      )
      .toBeGreaterThan(t0);
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
  }

  async function playAndMark(page, concertId, trackIdx) {
    await playTrack(page, concertId, trackIdx);
    await page.evaluate(() => {
      window.__noReload = true;
      // Prevent native track-end from firing auto-advance side effects while
      // we click delete buttons; mirrors the sidebar.spec.js pattern.
      document.getElementById("player-audio").loop = true;
    });
    return page.evaluate(() => document.getElementById("player-audio").currentTime);
  }

  test("deleting another concert's track does not stop playback", async ({
    page,
  }) => {
    const t0 = await playAndMark(page, AUDIO, 0);
    // Per-track delete swaps the whole SECOND card (hx-target="closest .card").
    await openTracks(page, SECOND);
    // Use evaluate to avoid real pointer events being intercepted by the fixed
    // player bar when the card is low in the grid.
    await page
      .locator(
        `#concert-${SECOND} button.btn-delete[hx-post$="/tracks/0/delete"]`
      )
      .evaluate(el => el.click());

    await expect(
      page.locator(
        `#concert-${SECOND} button.btn-delete[hx-post$="/tracks/0/delete"]`
      )
    ).toHaveCount(0);
    await expectPlayerStillPlaying(page, t0);
  });

  test("deleting another concert's download does not stop playback", async ({
    page,
  }) => {
    const t0 = await playAndMark(page, AUDIO, 0);
    // Use evaluate to avoid real pointer events being intercepted by the fixed
    // player bar when the card is low in the grid.
    await page
      .locator(`#concert-${SECOND} button[title="Delete downloaded file"]`)
      .evaluate(el => el.click());

    await expect(
      page.locator(`#concert-${SECOND} button[title="Delete downloaded file"]`)
    ).toHaveCount(0);
    await expectPlayerStillPlaying(page, t0);
  });

  test("deleting a track on the currently-playing concert's own card keeps playback going", async ({
    page,
  }) => {
    const t0 = await playAndMark(page, SECOND, 0);
    // Delete a *different* track of the playing concert: the whole card swaps
    // while its track 0 keeps playing in the persistent player.
    // Use evaluate to avoid real pointer events being intercepted by the fixed
    // player bar when the card is low in the grid.
    await page
      .locator(
        `#concert-${SECOND} button.btn-delete[hx-post$="/tracks/1/delete"]`
      )
      .evaluate(el => el.click());

    await expect(
      page.locator(
        `#concert-${SECOND} button.btn-delete[hx-post$="/tracks/1/delete"]`
      )
    ).toHaveCount(0);
    await expectPlayerStillPlaying(page, t0);
  });
});
