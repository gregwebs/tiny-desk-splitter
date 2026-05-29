const { test, expect } = require("@playwright/test");

function generateSilentWav(durationSec) {
  const sampleRate = 8000;
  const bitsPerSample = 16;
  const channels = 1;
  const numSamples = sampleRate * durationSec;
  const dataSize = numSamples * channels * (bitsPerSample / 8);
  const buffer = Buffer.alloc(44 + dataSize);

  buffer.write("RIFF", 0);
  buffer.writeUInt32LE(36 + dataSize, 4);
  buffer.write("WAVE", 8);
  buffer.write("fmt ", 12);
  buffer.writeUInt32LE(16, 16); // fmt chunk size
  buffer.writeUInt16LE(1, 20); // PCM
  buffer.writeUInt16LE(channels, 22);
  buffer.writeUInt32LE(sampleRate, 24);
  buffer.writeUInt32LE(sampleRate * channels * (bitsPerSample / 8), 28);
  buffer.writeUInt16LE(channels * (bitsPerSample / 8), 32);
  buffer.writeUInt16LE(bitsPerSample, 34);
  buffer.write("data", 36);
  buffer.writeUInt32LE(dataSize, 40);
  // PCM data is already zeros (silence)

  return buffer;
}

const SILENCE_WAV = generateSilentWav(10);

function mockMediaInfo(page) {
  return page.route("**/tracks/*/media-info", async (route) => {
    const url = route.request().url();
    const match = url.match(/concerts\/(\d+)\/tracks\/(\d+)\/media-info/);
    if (!match) {
      await route.fallback();
      return;
    }
    const [, concertId, trackIdx] = match;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        url: "/test-audio/silence.wav",
        title: `Track ${trackIdx} of C${concertId}`,
        artist: `Artist ${concertId}`,
        is_video: false,
        playable: true,
        track_index: parseInt(trackIdx),
        // The mock concert is effectively endless, so there is always a next track.
        has_next: true,
        liked: false,
      }),
    });
  });
}

function mockNextMediaInfo(page) {
  return page.route("**/tracks/*/next-media-info", async (route) => {
    const url = route.request().url();
    const match = url.match(
      /concerts\/(\d+)\/tracks\/(\d+)\/next-media-info/
    );
    if (!match) {
      await route.fallback();
      return;
    }
    const [, concertId, trackIdx] = match;
    const nextIdx = parseInt(trackIdx) + 1;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        url: "/test-audio/silence.wav",
        title: `Track ${nextIdx} of C${concertId}`,
        artist: `Artist ${concertId}`,
        is_video: false,
        playable: true,
        track_index: nextIdx,
        has_next: true,
        liked: false,
      }),
    });
  });
}

function mockAudioFile(page) {
  return page.route("**/test-audio/**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "audio/wav",
      body: SILENCE_WAV,
    });
  });
}

function mockListenPost(page) {
  return page.route("**/listen", async (route) => {
    if (route.request().method() === "POST") {
      await route.fulfill({ status: 200, body: "" });
    } else {
      await route.fallback();
    }
  });
}

async function expandTracks(page, concertId) {
  const toggle = page.locator(
    `#concert-${concertId} button[onclick*="toggleTracks"]`
  );
  if (await toggle.count()) {
    await toggle.click();
    await page.waitForSelector(
      `[data-concert-id="${concertId}"][data-track-idx="0"]`
    );
  }
}

function trackButton(page, concertId, trackIdx) {
  return page.locator(
    `[data-concert-id="${concertId}"][data-track-idx="${trackIdx}"]`
  );
}

async function waitForPlaying(page) {
  await page.waitForFunction(() => {
    const a = document.getElementById("player-audio");
    return a && !a.paused;
  });
}

async function simulateTrackEnd(page) {
  await page.evaluate(() => {
    const a = document.getElementById("player-audio");
    a.pause();
    a.dispatchEvent(new Event("ended"));
  });
}

// Override a track's media-info so it reports as a (playable) video, then start
// it playing by clicking its track button.
async function playVideoTrack(page, concertId, trackIdx, title = "Video track") {
  await page.route(
    `**/concerts/${concertId}/tracks/${trackIdx}/media-info`,
    async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title,
          artist: `Artist ${concertId}`,
          is_video: true,
          playable: true,
          track_index: trackIdx,
          has_next: true,
        }),
      });
    }
  );
  await expandTracks(page, concertId);
  await trackButton(page, concertId, trackIdx).click();
  await waitForPlaying(page);
}

test.describe("Player Queue", () => {
  test.beforeEach(async ({ page }) => {
    await mockAudioFile(page);
    await mockMediaInfo(page);
    await mockNextMediaInfo(page);
    await mockListenPost(page);
    await page.goto("/");
  });

  test("clicking a track with nothing playing starts playback immediately", async ({
    page,
  }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();

    await waitForPlaying(page);
    await expect(page.locator("#player-bar")).toHaveClass(/active/);
    await expect(page.locator("#player-title")).toHaveText("Track 0 of C2");
    // track_index 0 is the first track, shown as #1
    await expect(page.locator("#player-track")).toHaveText("#1");
    await expect(page.locator("#player-track")).toBeVisible();
    await expect(page.locator("#player-queue-badge")).toBeHidden();
  });

  test("Next button is disabled when nothing is next to play", async ({
    page,
  }) => {
    // Report this track as the last one: no following track in the concert.
    await page.route("**/concerts/2/tracks/0/media-info", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title: "Last track",
          artist: "Artist 2",
          is_video: false,
          playable: true,
          track_index: 0,
          has_next: false,
        }),
      });
    });

    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expect(page.locator("#player-next")).toBeDisabled();

    // The guard must also stop the public skip API from pausing the track.
    await page.evaluate(() => Player.skipToNext());
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(false);
  });

  test("Next button re-enables once a track is queued", async ({ page }) => {
    await page.route("**/concerts/2/tracks/0/media-info", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title: "Last track",
          artist: "Artist 2",
          is_video: false,
          playable: true,
          track_index: 0,
          has_next: false,
        }),
      });
    });

    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);
    await expect(page.locator("#player-next")).toBeDisabled();

    // Queue another track — Next now has something to advance to.
    await expandTracks(page, 3);
    await trackButton(page, 3, 0).click();
    await expect(page.locator("#player-next")).toBeEnabled();
  });

  test("clicking a track while playing enqueues it", async ({ page }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expandTracks(page, 3);
    await trackButton(page, 3, 0).click();

    await expect(page.locator("#player-queue-badge")).toBeVisible();
    await expect(page.locator("#player-queue-badge")).toHaveText("1");
    await expect(page.locator("#player-title")).toHaveText("Track 0 of C2");
  });

  test("multiple tracks can be queued", async ({ page }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expandTracks(page, 3);
    await trackButton(page, 3, 0).click();
    await trackButton(page, 3, 1).click();

    await expect(page.locator("#player-queue-badge")).toHaveText("2");
  });

  test("duplicate tracks are not enqueued", async ({ page }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expandTracks(page, 3);
    await trackButton(page, 3, 0).click();
    await trackButton(page, 3, 0).click();

    await expect(page.locator("#player-queue-badge")).toHaveText("1");
  });

  test("clicking currently-playing track toggles pause", async ({ page }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    // Click the same track again — should pause
    await trackButton(page, 2, 0).click();
    await page.waitForFunction(() => {
      const a = document.getElementById("player-audio");
      return a && a.paused;
    });

    // Click again — should resume
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);
  });

  test("next button plays from queue", async ({ page }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expandTracks(page, 3);
    await trackButton(page, 3, 0).click();
    await expect(page.locator("#player-queue-badge")).toHaveText("1");

    await page.locator("#player-next").click();

    await expect(page.locator("#player-title")).toHaveText("Track 0 of C3");
    await expect(page.locator("#player-queue-badge")).toBeHidden();
  });

  test("next button auto-advances in concert when queue is empty", async ({
    page,
  }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await page.locator("#player-next").click();

    await expect(page.locator("#player-title")).toHaveText("Track 1 of C2");
  });

  test("when track ends, queued song plays instead of auto-advance", async ({
    page,
  }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expandTracks(page, 3);
    await trackButton(page, 3, 1).click();
    await expect(page.locator("#player-queue-badge")).toHaveText("1");

    await simulateTrackEnd(page);

    await expect(page.locator("#player-title")).toHaveText("Track 1 of C3", {
      timeout: 5000,
    });
    await expect(page.locator("#player-queue-badge")).toBeHidden();
  });

  test("after queue drains, auto-advance follows last queued concert", async ({
    page,
  }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expandTracks(page, 3);
    await trackButton(page, 3, 1).click();

    // Skip to the queued track (concert 3, track 1)
    await page.locator("#player-next").click();
    await expect(page.locator("#player-title")).toHaveText("Track 1 of C3");

    // Now skip again — queue is empty, should auto-advance in concert 3
    await page.locator("#player-next").click();
    await expect(page.locator("#player-title")).toHaveText("Track 2 of C3");
  });

  test("queue badge tooltip shows queued track titles", async ({ page }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expandTracks(page, 3);
    await trackButton(page, 3, 0).click();
    await trackButton(page, 3, 1).click();

    const badge = page.locator("#player-queue-badge");
    await expect(badge).toHaveText("2");
    const title = await badge.getAttribute("title");
    expect(title).toBeTruthy();
    expect(title.split("\n")).toHaveLength(2);
  });
});

test.describe("Inline video", () => {
  test.beforeEach(async ({ page }) => {
    await mockAudioFile(page);
    await mockMediaInfo(page);
    await mockNextMediaInfo(page);
    await mockListenPost(page);
    await page.goto("/");
  });

  test("Watch and Open buttons are hidden for audio-only tracks", async ({
    page,
  }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click(); // default mock: is_video false
    await waitForPlaying(page);

    await expect(page.locator("#player-watch")).toBeHidden();
    await expect(page.locator("#player-open")).toBeHidden();
  });

  test("Watch and Open buttons are shown for video tracks", async ({ page }) => {
    await playVideoTrack(page, 2, 0);

    await expect(page.locator("#player-watch")).toBeVisible();
    await expect(page.locator("#player-open")).toBeVisible();
  });

  test("Watch button folds the video panel open and closed", async ({ page }) => {
    await playVideoTrack(page, 2, 0);
    const panel = page.locator("#player-video-panel");
    await expect(panel).not.toHaveClass(/open/);

    await page.locator("#player-watch").click();
    await expect(panel).toHaveClass(/open/);
    // Revealing the video must not interrupt playback.
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
    await playVideoTrack(page, 2, 0);
    await page.waitForFunction(
      () => document.getElementById("player-audio").currentTime > 0.1
    );

    await page.locator("#player-watch").click(); // open
    await page.locator("#player-watch").click(); // close

    // Same element throughout, so position continues rather than resetting.
    const t = await page.evaluate(
      () => document.getElementById("player-audio").currentTime
    );
    expect(t).toBeGreaterThan(0);
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("Open button posts to the watch endpoint", async ({ page }) => {
    let watchPosted = false;
    await page.route("**/concerts/2/tracks/0/watch", async (route) => {
      if (route.request().method() === "POST") watchPosted = true;
      await route.fulfill({ status: 200, body: "" });
    });

    await playVideoTrack(page, 2, 0);
    await page.locator("#player-open").click();

    await expect.poll(() => watchPosted).toBe(true);
  });

  test("Open pauses the JS player so audio doesn't double up", async ({
    page,
  }) => {
    await page.route("**/concerts/2/tracks/0/watch", (route) =>
      route.fulfill({ status: 200, body: "" })
    );

    await playVideoTrack(page, 2, 0);
    await page.locator("#player-open").click();

    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
  });

  test("Open still pauses even when the watch POST fails", async ({ page }) => {
    await page.route("**/concerts/2/tracks/0/watch", (route) => route.abort());

    await playVideoTrack(page, 2, 0);
    await page.locator("#player-open").click();

    // Playback is paused up front, independent of the POST result.
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
    await page.route("**/concerts/2/tracks/0/media-info", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title: "Video track",
          artist: "Artist 2",
          is_video: true,
          playable: true,
          track_index: 0,
          has_next: true,
        }),
      });
    });

    await page.evaluate(() =>
      Player.watchTrackDirect(document.createElement("button"), 2, 0)
    );
    await waitForPlaying(page);

    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("inline Watch of a non-playable file does not open the panel", async ({
    page,
  }) => {
    await page.route("**/concerts/2/tracks/0/media-info", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title: "Unplayable",
          artist: "Artist 2",
          is_video: true,
          playable: false,
          track_index: 0,
          has_next: false,
        }),
      });
    });
    await page.evaluate(() => {
      window.__opened = null;
      window.open = (u) => {
        window.__opened = u;
      };
    });

    await page.evaluate(() =>
      Player.watchTrackDirect(document.createElement("button"), 2, 0)
    );

    await expect
      .poll(() => page.evaluate(() => window.__opened))
      .toBe("/test-audio/silence.wav");
    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
  });

  test("auto-advancing from video to audio collapses the panel", async ({
    page,
  }) => {
    await page.route(
      "**/concerts/2/tracks/0/next-media-info",
      async (route) => {
        await route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({
            url: "/test-audio/silence.wav",
            title: "Audio next",
            artist: "Artist 2",
            is_video: false,
            playable: true,
            track_index: 1,
            has_next: true,
          }),
        });
      }
    );

    await playVideoTrack(page, 2, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await simulateTrackEnd(page);

    await expect(page.locator("#player-title")).toHaveText("Audio next");
    await expect(page.locator("#player-video-panel")).not.toHaveClass(/open/);
    await expect(page.locator("#player-watch")).toBeHidden();
  });

  test("auto-advancing from video to video keeps the panel open", async ({
    page,
  }) => {
    await page.route(
      "**/concerts/2/tracks/0/next-media-info",
      async (route) => {
        await route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({
            url: "/test-audio/silence.wav",
            title: "Video next",
            artist: "Artist 2",
            is_video: true,
            playable: true,
            track_index: 1,
            has_next: true,
          }),
        });
      }
    );

    await playVideoTrack(page, 2, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await simulateTrackEnd(page);

    await expect(page.locator("#player-title")).toHaveText("Video next");
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });

  test("video panel stays open across Back/Forward navigation", async ({
    page,
  }) => {
    await page.route("**/listen", (r) => r.fulfill({ status: 200, body: "" }));
    await playVideoTrack(page, 2, 0);
    await page.locator("#player-watch").click();
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);

    await page.locator('header a[href="/settings"]').click();
    await page.waitForFunction(() => location.pathname === "/settings");
    await page.goBack();
    await page.waitForFunction(() => location.pathname === "/");

    // #player-video-panel lives inside the hx-preserve'd #player-container.
    await expect(page.locator("#player-video-panel")).toHaveClass(/open/);
  });
});

// The track-list like button (☆/★) lives in the same <li> as the track's
// listen button, before it.
function trackListLikeButton(page, concertId, trackIdx) {
  return page
    .locator("li")
    .filter({ has: trackButton(page, concertId, trackIdx) })
    .locator(".btn-like");
}

// Override a track's media-info to report a specific liked state, then play it.
async function playTrackWithLiked(page, concertId, trackIdx, liked) {
  await page.route(
    `**/concerts/${concertId}/tracks/${trackIdx}/media-info`,
    async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title: `Track ${trackIdx} of C${concertId}`,
          artist: `Artist ${concertId}`,
          is_video: false,
          playable: true,
          track_index: trackIdx,
          has_next: true,
          liked,
        }),
      });
    }
  );
  await expandTracks(page, concertId);
  await trackButton(page, concertId, trackIdx).click();
  await waitForPlaying(page);
}

test.describe("Player like star", () => {
  test.beforeEach(async ({ page }) => {
    await mockAudioFile(page);
    await mockMediaInfo(page);
    await mockNextMediaInfo(page);
    await mockListenPost(page);
    await page.goto("/");
  });

  test("star is shown and reflects an unliked track", async ({ page }) => {
    await playTrackWithLiked(page, 2, 0, false);

    const star = page.locator("#player-like");
    await expect(star).toBeVisible();
    await expect(star).toHaveText("☆");
    await expect(star).not.toHaveClass(/liked/);
  });

  test("star reflects a liked track", async ({ page }) => {
    await playTrackWithLiked(page, 2, 0, true);

    const star = page.locator("#player-like");
    await expect(star).toBeVisible();
    await expect(star).toHaveText("★");
    await expect(star).toHaveClass(/liked/);
  });

  test("star is hidden during whole-album playback", async ({ page }) => {
    // Album media-info (no /tracks/ segment) → track_index null → star hidden.
    await page.route("**/concerts/2/media-info", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title: "Whole album",
          artist: "Artist 2",
          is_video: false,
          playable: true,
          track_index: null,
          has_next: false,
          liked: false,
        }),
      });
    });

    await page.evaluate(() =>
      Player.playAlbum(document.createElement("button"), 2)
    );
    await waitForPlaying(page);

    await expect(page.locator("#player-like")).toBeHidden();
  });

  test("clicking the star POSTs to /like and flips the star and the track-list button", async ({
    page,
  }) => {
    let likePosted = false;
    await page.route("**/concerts/2/tracks/0/like", async (route) => {
      if (route.request().method() === "POST") likePosted = true;
      // The player ignores the body; a 200 is enough for the optimistic update.
      await route.fulfill({ status: 200, body: "" });
    });

    await playTrackWithLiked(page, 2, 0, false);
    const star = page.locator("#player-like");
    await expect(star).toHaveText("☆");

    await star.click();

    await expect.poll(() => likePosted).toBe(true);
    await expect(star).toHaveText("★");
    await expect(star).toHaveClass(/liked/);
    // The on-page track-list button mirrors the new state in place.
    await expect(trackListLikeButton(page, 2, 0)).toHaveClass(/liked/);
  });

  test("a failing /like POST reverts the star", async ({ page }) => {
    await page.route("**/concerts/2/tracks/0/like", (route) => route.abort());

    await playTrackWithLiked(page, 2, 0, false);
    const star = page.locator("#player-like");
    await expect(star).toHaveText("☆");

    await star.click();

    // Optimistic flip is rolled back once the POST fails.
    await expect(star).toHaveText("☆");
    await expect(star).not.toHaveClass(/liked/);
    await expect(trackListLikeButton(page, 2, 0)).not.toHaveClass(/liked/);
  });

  test("toggling the track-list star updates the player star (reverse sync)", async ({
    page,
  }) => {
    // Uses the real /like endpoint so the swapped-in track list reflects the
    // toggled state; the assertion is relational so it is independent of the
    // track's initial liked state in the DB.
    await playTrackWithLiked(page, 2, 0, false);

    const listBtn = trackListLikeButton(page, 2, 0);
    await listBtn.click();
    // After the htmx swap, the player star must match the track-list button.
    await expect
      .poll(async () => {
        const listLiked = await trackListLikeButton(page, 2, 0).evaluate((el) =>
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
    await playTrackWithLiked(page, 2, 0, false);
    const playing = trackListLikeButton(page, 2, 0);
    const playingBefore = await playing.evaluate((el) =>
      el.classList.contains("liked")
    );

    // Toggle the like on an unrelated concert's track (real endpoint + swap).
    await expandTracks(page, 3);
    await trackListLikeButton(page, 3, 0).click();
    await expect(trackListLikeButton(page, 3, 0)).toBeVisible();

    // The playing track (concert 2/0) must be untouched by concert 3's toggle,
    // and the player star stays in sync with the playing track's button.
    expect(await playing.evaluate((el) => el.classList.contains("liked"))).toBe(
      playingBefore
    );
    await expect
      .poll(() =>
        page
          .locator("#player-like")
          .evaluate((el) => el.classList.contains("liked"))
      )
      .toBe(playingBefore);
  });
});

// Mock the track delete endpoint, returning a track-list fragment containing the
// given remaining tracks (with the data attributes findTrackButton relies on).
function mockDeletePost(page, concertId, trackIdx, remaining, opts = {}) {
  const state = { hits: 0 };
  page.route(
    `**/concerts/${concertId}/tracks/${trackIdx}/delete`,
    async (route) => {
      if (route.request().method() !== "POST") return route.fallback();
      state.hits += 1;
      if (opts.abort) return route.abort();
      if (opts.delayMs) await new Promise((r) => setTimeout(r, opts.delayMs));
      const items = remaining
        .map(
          (t) => `<li>
            <button class="btn-like" title="Like">☆</button>
            <button class="btn-track-listen" data-concert-id="${concertId}" data-track-idx="${t.index}" onclick="Player.playTrack(this, ${concertId}, ${t.index})">${t.title}</button>
            <button class="btn-delete" hx-post="/concerts/${concertId}/tracks/${t.index}/delete" hx-target="closest .track-list" hx-swap="outerHTML"><span class="icon-trash"></span></button>
          </li>`
        )
        .join("");
      await route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: `<ol class="track-list">${items}</ol>`,
      });
    }
  );
  return state;
}

test.describe("Player delete", () => {
  test.beforeEach(async ({ page }) => {
    await mockAudioFile(page);
    await mockMediaInfo(page);
    await mockNextMediaInfo(page);
    await mockListenPost(page);
    await page.goto("/");
  });

  test("delete button is shown when a track is playing", async ({ page }) => {
    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await expect(page.locator("#player-delete")).toBeVisible();
  });

  test("delete button is hidden during whole-album playback", async ({
    page,
  }) => {
    await page.route("**/concerts/2/media-info", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          url: "/test-audio/silence.wav",
          title: "Whole album",
          artist: "Artist 2",
          is_video: false,
          playable: true,
          track_index: null,
          has_next: false,
          liked: false,
        }),
      });
    });

    await page.evaluate(() =>
      Player.playAlbum(document.createElement("button"), 2)
    );
    await waitForPlaying(page);

    await expect(page.locator("#player-delete")).toBeHidden();
  });

  test("clicking delete posts to the delete endpoint and advances to the next track", async ({
    page,
  }) => {
    const del = mockDeletePost(page, 2, 0, [
      { index: 1, title: "Limbo" },
      { index: 2, title: "track4" },
    ]);

    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await page.locator("#player-delete").click();

    await expect.poll(() => del.hits).toBe(1);
    await expect(page.locator("#player-title")).toHaveText("Track 1 of C2");
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("deleting the last track stops playback", async ({ page }) => {
    // No track after this one: next-media-info 404s, so the player stops.
    await page.route("**/concerts/2/tracks/0/next-media-info", (route) =>
      route.fulfill({ status: 404, body: "" })
    );
    mockDeletePost(page, 2, 0, []);

    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await page.locator("#player-delete").click();

    await expect(page.locator("#player-bar")).not.toHaveClass(/active/);
    await expect(page.locator("#player-delete")).toBeHidden();
    await expect
      .poll(() => page.evaluate(() => document.getElementById("player-audio").paused))
      .toBe(true);
  });

  test("a delete that completes after playback moved on does not interrupt the new track", async ({
    page,
  }) => {
    // The delete POST is slow; the track ends mid-flight and auto-advances. When
    // the delete finally resolves it must not disturb the now-playing track.
    mockDeletePost(page, 2, 0, [{ index: 1, title: "Limbo" }], { delayMs: 600 });

    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await page.locator("#player-delete").click();
    // While the delete is in flight, the current track ends and auto-advances.
    await simulateTrackEnd(page);
    await expect(page.locator("#player-title")).toHaveText("Track 1 of C2");

    // Wait past the delete's delay; the resolved delete must leave track 1 alone.
    await page.waitForTimeout(800);
    await expect(page.locator("#player-title")).toHaveText("Track 1 of C2");
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("a failing delete keeps playback going and shows an error", async ({
    page,
  }) => {
    mockDeletePost(page, 2, 0, [], { abort: true });

    await expandTracks(page, 2);
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await page.locator("#player-delete").click();

    await expect(page.locator("#player-error")).toBeVisible();
    await expect(page.locator("#player-title")).toHaveText("Track 0 of C2");
    expect(
      await page.evaluate(() => document.getElementById("player-audio").paused)
    ).toBe(false);
  });

  test("the on-page track list is refreshed when a track is deleted", async ({
    page,
  }) => {
    // Returned fragment omits track 0, so its listen button disappears.
    mockDeletePost(page, 2, 0, [
      { index: 1, title: "Limbo" },
      { index: 2, title: "track4" },
    ]);

    await expandTracks(page, 2);
    await expect(trackButton(page, 2, 0)).toBeVisible();
    await trackButton(page, 2, 0).click();
    await waitForPlaying(page);

    await page.locator("#player-delete").click();

    await expect(trackButton(page, 2, 0)).toHaveCount(0);
    await expect(trackButton(page, 2, 1)).toBeVisible();
  });
});

// The track-list delete (trash) button lives in the same <li> as the track's
// listen button, after it.
function trackListDeleteButton(page, concertId, trackIdx) {
  return page
    .locator("li")
    .filter({ has: trackButton(page, concertId, trackIdx) })
    .locator(".btn-delete");
}

test.describe("Starred tracks hide the delete button", () => {
  test.beforeEach(async ({ page }) => {
    await mockAudioFile(page);
    await mockMediaInfo(page);
    await mockNextMediaInfo(page);
    await mockListenPost(page);
    await page.goto("/");
  });

  test("player: starring the playing track hides #player-delete, unstarring restores it", async ({
    page,
  }) => {
    await page.route("**/concerts/2/tracks/0/like", (route) =>
      route.fulfill({ status: 200, body: "" })
    );

    await playTrackWithLiked(page, 2, 0, false);
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
    await playTrackWithLiked(page, 2, 0, true);

    await expect(page.locator("#player-like")).toHaveText("★");
    await expect(page.locator("#player-delete")).toBeHidden();
  });

  test("track list: starring the playing track from the player hides its row delete button", async ({
    page,
  }) => {
    await page.route("**/concerts/2/tracks/0/like", (route) =>
      route.fulfill({ status: 200, body: "" })
    );

    await playTrackWithLiked(page, 2, 0, false);
    const rowDelete = trackListDeleteButton(page, 2, 0);

    // Star via the player — the in-place class flip drives the CSS :has() rule.
    await page.locator("#player-like").click();
    await expect(rowDelete).toBeHidden();
    await expect(page.locator("#player-delete")).toBeHidden();

    // Unstar — both delete buttons come back.
    await page.locator("#player-like").click();
    await expect(rowDelete).toBeVisible();
    await expect(page.locator("#player-delete")).toBeVisible();
  });

  test("track list: a row's delete visibility tracks its own star (htmx re-render)", async ({
    page,
  }) => {
    // Uses the real /like endpoint so the swapped-in row reflects the toggle.
    // Assertion is relational, so it is independent of the track's initial DB state.
    await expandTracks(page, 2);
    const relationHolds = async () => {
      const liked = await trackListLikeButton(page, 2, 0).evaluate((el) =>
        el.classList.contains("liked")
      );
      const hidden = await trackListDeleteButton(page, 2, 0).evaluate(
        (el) => getComputedStyle(el).display === "none"
      );
      return liked === hidden; // starred iff delete hidden
    };

    await trackListLikeButton(page, 2, 0).click();
    await expect.poll(relationHolds).toBe(true);

    await trackListLikeButton(page, 2, 0).click();
    await expect.poll(relationHolds).toBe(true);
  });
});

test.describe("Deleting a download/split keeps the player playing", () => {
  test.beforeEach(async ({ page }) => {
    await mockAudioFile(page);
    await mockMediaInfo(page);
    await mockNextMediaInfo(page);
    await mockListenPost(page);
    await page.goto("/");
  });

  // Assert the player survived a card swap (no full page reload): a window
  // sentinel set after load persists, audio is still playing, currentTime keeps
  // advancing (not reset by node recreation), and the bar stays active.
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

  async function playConcertTrack(page, concertId, trackIdx) {
    await expandTracks(page, concertId);
    await trackButton(page, concertId, trackIdx).click();
    await waitForPlaying(page);
    await page.evaluate(() => {
      window.__noReload = true;
    });
    return page.evaluate(() => document.getElementById("player-audio").currentTime);
  }

  test("deleting another concert's tracks (delete-split) does not stop playback", async ({
    page,
  }) => {
    // Card-swap response (its trash targets `closest .card`); no HX-Refresh.
    await page.route("**/concerts/2/delete-split", (route) =>
      route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: `<div class="card status-downloaded" id="concert-2"><div class="card-body">card updated</div></div>`,
      })
    );

    const t0 = await playConcertTrack(page, 3, 0);
    await page.locator('#concert-2 button[title="Clear split record"]').click();

    await expect(page.locator("#concert-2")).toContainText("card updated");
    await expectPlayerStillPlaying(page, t0);
  });

  test("deleting another concert's download does not stop playback", async ({
    page,
  }) => {
    // delete-download's trash targets `this`; the response retargets the card.
    await page.route("**/concerts/2/delete-download", (route) =>
      route.fulfill({
        status: 200,
        headers: {
          "content-type": "text/html; charset=utf-8",
          "HX-Retarget": "#concert-2",
          "HX-Reswap": "outerHTML",
        },
        body: `<div class="card status-available" id="concert-2"><div class="card-body">card updated</div></div>`,
      })
    );

    const t0 = await playConcertTrack(page, 3, 0);
    await page.locator('#concert-2 button[title="Delete downloaded file"]').click();

    await expect(page.locator("#concert-2")).toContainText("card updated");
    await expectPlayerStillPlaying(page, t0);
  });

  test("deleting the currently-playing concert's own card keeps playback going", async ({
    page,
  }) => {
    await page.route("**/concerts/2/delete-split", (route) =>
      route.fulfill({
        status: 200,
        contentType: "text/html; charset=utf-8",
        body: `<div class="card status-downloaded" id="concert-2"><div class="card-body">card updated</div></div>`,
      })
    );

    const t0 = await playConcertTrack(page, 2, 0);
    await page.locator('#concert-2 button[title="Clear split record"]').click();

    await expect(page.locator("#concert-2")).toContainText("card updated");
    await expectPlayerStillPlaying(page, t0);
  });
});
