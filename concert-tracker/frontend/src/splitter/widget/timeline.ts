import { Option } from "effect";

import { clamp } from "../core";

/** Shared by `view.ts` (sets the attribute, looks the element up for
 *  click-to-audition) and `subscription.ts` (looks it up on every drag
 *  pointermove). Scoped by concert id rather than a fixed id so a second
 *  embed of this widget — unlikely today, but cheap to make safe — can't
 *  collide with a stale lookup. */
export const TIMELINE_DATA_ATTRIBUTE = "splitter-timeline-id";

export const timelineElement = (concertId: number): Option.Option<HTMLElement> =>
  Option.fromNullishOr(
    document.querySelector<HTMLElement>(`[data-${TIMELINE_DATA_ATTRIBUTE}="${concertId}"]`),
  );

/** Converts a viewport-relative `clientX` into a time in seconds, by
 *  re-measuring the timeline element's current layout rather than relying on
 *  a rect cached at drag start — robust against layout shifts mid-drag. */
export const timeFromClientX = (clientX: number, timeline: HTMLElement, duration: number): number => {
  const rect = timeline.getBoundingClientRect();
  if (rect.width === 0) {
    return 0;
  }
  return clamp((clientX - rect.left) / rect.width, 0, 1) * duration;
};
