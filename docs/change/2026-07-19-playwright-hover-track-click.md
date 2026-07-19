# Stabilize the hover-only track click

Issue [#130](https://github.com/gregwebs/tiny-desk-splitter/issues/130)
reported an intermittent failure in the Playwright automated-splitting test.
This change affects test interaction only; product behavior and application
state transitions are unchanged.

## Root cause

Listing-card tracks are visible only while the card matches CSS `:hover`.
Playwright's pointer-driven `locator.click()` can disturb that hover state
while scrolling and checking actionability. The track list then disappears,
the card teaser returns, and the teaser intercepts the pending click.

The adjacent tracks-button scenario already uses a DOM-dispatched click for
the same single-process hover quirk. The failing track-button scenario now
uses that established interaction pattern too.

```text
Before

card hovered -> tracks visible -> pointer click moves -> hover lost
             -> tracks hidden -> teaser intercepts click -> timeout

After

card hovered -> tracks visible -> DOM click -> prepare -> split -> autoplay
```

## Implementation plan

- [x] Keep the existing end-to-end scenario as the regression seam.
- [x] Dispatch the hover-only track button's click through the DOM.
- [x] Attempt the focused automated-splitting scenario repeatedly locally.
- [x] Record the local Chromium launch limitation for Playwright verification.
- [x] Obtain an engineering-lead review.
- [x] Verify the pull request's Playwright CI job passes.

## Verification plan

The existing test verifies the complete user-visible state sequence:

```text
unsplit track -> preparing indicator -> player reports preparation
              -> split completes -> selected track autoplays
              -> card reports all tracks available
```

Run the focused scenario repeatedly to exercise the former flaky boundary,
then run the full Playwright suite to check for interaction regressions.

## Verification results

- `just test-ts`: passed (68 Node tests and 249 frontend tests).
- `just lint`: Rust formatting and Clippy passed; the suite then stopped in
  `scripts/shellcheck.sh` because the host's Bash lacks `mapfile`.
- Focused Playwright, three repetitions: the application scenario did not
  start because this macOS execution host's Chromium process exited with
  `SIGTRAP` during browser launch. This is the same host limitation observed
  during investigation and is independent of the changed test interaction.
- Initial adversarial engineering-lead review: approved with no code findings.
- Linux Playwright CI: passed in
  [run 29687340937](https://github.com/gregwebs/tiny-desk-splitter/actions/runs/29687340937/job/88193864651).
