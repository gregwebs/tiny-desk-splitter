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
