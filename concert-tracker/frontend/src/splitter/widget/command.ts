import { Effect, Option, Schema as S } from "effect";
import { Command, Port } from "foldkit";

import {
  getJson,
  getJsonOrNull,
  sendJson,
  type MediaInfo,
  type SplitTimestampsResponse,
} from "../../api/client";
import { initState } from "../core";
import {
  CompletedEmitAuditionAt,
  CompletedResetSplit,
  CompletedResync,
  CompletedRevertEdits,
  CompletedSubmitSplit,
  FailedFetchSplitterData,
  RevertOutcomeValue,
  SucceededFetchSplitterData,
} from "./message";
import { ports } from "./port";

// COMMAND

export const FetchSplitterData = Command.define(
  "FetchSplitterData",
  { concertId: S.Number },
  SucceededFetchSplitterData,
  FailedFetchSplitterData,
)(({ concertId }) =>
  Effect.gen(function* () {
    const [timestampsResponse, mediaInfo] = yield* Effect.all(
      [
        Effect.tryPromise(() =>
          getJson<SplitTimestampsResponse>(`/concerts/${concertId}/split-timestamps`),
        ),
        Effect.tryPromise(() => getJsonOrNull<MediaInfo>(`/concerts/${concertId}/media-info`)),
      ],
      { concurrency: "unbounded" },
    );
    const maybeMediaInfo = Option.fromNullishOr(mediaInfo);
    return SucceededFetchSplitterData({
      maybeEditor: Option.fromNullishOr(initState(timestampsResponse)),
      maybeMediaUrl: Option.flatMap(maybeMediaInfo, (info) => Option.fromNullishOr(info.url)),
      playable: Option.match(maybeMediaInfo, {
        onNone: () => false,
        onSome: (info) => !!info.playable,
      }),
    });
  }).pipe(Effect.catch(() => Effect.succeed(FailedFetchSplitterData()))),
);

const TimestampPayloadSchema = S.Struct({
  songs: S.Array(
    S.Struct({ title: S.String, start_time: S.Number, end_time: S.Number }),
  ),
});

/** Shared by `SubmitSplit` and `ResetSplit`: POST, emit `cardDirty` on a
 *  queued (202) response, and fold any status code or network failure into
 *  `{ status, body }` rather than an Effect failure channel — mirrors the old
 *  `postJob` helper's uniform status-code branching (202 queued, 200 no-op,
 *  409 busy, 422 validation, ...), now read by `update.ts`. */
const postSplitJob = (
  url: string,
  body: unknown,
): Effect.Effect<{ status: number; body: string }> =>
  Effect.gen(function* () {
    const response = yield* Effect.tryPromise(() => sendJson(url, body, "POST"));
    if (response.status === 202) {
      yield* Port.emit(ports.outbound.cardDirty, undefined);
    }
    const text = yield* Effect.tryPromise(() => response.text());
    return { status: response.status, body: text };
  }).pipe(
    Effect.catch(() =>
      Effect.succeed({ status: 0, body: "Network error — please retry." }),
    ),
  );

export const SubmitSplit = Command.define(
  "SubmitSplit",
  { concertId: S.Number, payload: TimestampPayloadSchema },
  CompletedSubmitSplit,
)(({ concertId, payload }) =>
  postSplitJob(`/concerts/${concertId}/split-timestamps`, payload).pipe(
    Effect.map(({ status, body }) => CompletedSubmitSplit({ status, body })),
  ),
);

export const ResetSplit = Command.define(
  "ResetSplit",
  { concertId: S.Number },
  CompletedResetSplit,
)(({ concertId }) =>
  postSplitJob(`/concerts/${concertId}/split-timestamps/reset`, undefined).pipe(
    Effect.map(({ status, body }) => CompletedResetSplit({ status, body })),
  ),
);

export const RevertEdits = Command.define(
  "RevertEdits",
  { concertId: S.Number },
  CompletedRevertEdits,
)(({ concertId }) =>
  Effect.tryPromise(() =>
    getJson<SplitTimestampsResponse>(`/concerts/${concertId}/split-timestamps`),
  ).pipe(
    Effect.map((response) => {
      const editor = initState(response);
      return CompletedRevertEdits({
        outcome:
          editor === null
            ? RevertOutcomeValue.NoSavedEditor()
            : RevertOutcomeValue.RestoredEditor({ editor }),
      });
    }),
    Effect.catch(() =>
      Effect.succeed(CompletedRevertEdits({ outcome: RevertOutcomeValue.RevertFetchFailed() })),
    ),
  ),
);

/** Re-fetches saved timestamps after a 409 (a split job is already running)
 *  so the editor reflects whatever that job is producing. Silent either way:
 *  no saved data and a network failure are both folded into `None`, matching
 *  the old `resync()`'s empty catch block. */
export const ResyncAfterConflict = Command.define(
  "ResyncAfterConflict",
  { concertId: S.Number },
  CompletedResync,
)(({ concertId }) =>
  Effect.tryPromise(() =>
    getJson<SplitTimestampsResponse>(`/concerts/${concertId}/split-timestamps`),
  ).pipe(
    Effect.map((response) =>
      CompletedResync({ maybeEditor: Option.fromNullishOr(initState(response)) }),
    ),
    Effect.catch(() => Effect.succeed(CompletedResync({ maybeEditor: Option.none() }))),
  ),
);

export const EmitAuditionAt = Command.define(
  "EmitAuditionAt",
  { time: S.Number },
  CompletedEmitAuditionAt,
)(({ time }) =>
  Port.emit(ports.outbound.auditionAt, time).pipe(Effect.as(CompletedEmitAuditionAt())),
);
