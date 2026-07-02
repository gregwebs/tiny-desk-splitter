import { Effect, Schema as S } from "effect";
import { Command, Dom, Port } from "foldkit";

import {
  addPlaylistItem,
  concertMembership,
  createPlaylist,
  listPlaylists,
  playlistNestedIn,
  readJson,
  removePlaylistItem,
  trackMembership,
  type CreatedPlaylistJson,
  type MembershipJson,
} from "../../api/client";
import { addItemBody, type Member, type PlaylistRef } from "../core";
import {
  CompletedFocusFilter,
  CompletedLoad,
  CompletedMutation,
  CompletedRequestClose,
  CompletedRequestNewName,
  CompletedScrollActiveIntoView,
  FailedLoad,
  FailedMutation,
} from "./message";
import { AddTarget, RowId } from "./model";
import { ports } from "./port";

// COMMAND

const fetchAllPlaylists = (): Promise<PlaylistRef[]> =>
  listPlaylists().then((entries) =>
    entries.map((e) => ({ id: e.playlist.id, name: e.playlist.name })),
  );

const fetchMembershipJson = (target: AddTarget): Promise<MembershipJson[]> => {
  switch (target.type) {
    case "track":
      return trackMembership(target.concertId, target.trackIndex);
    case "concert":
      return concertMembership(target.concertId);
    case "playlist":
      return playlistNestedIn(target.childPlaylistId);
  }
};

const fetchMembers = (target: AddTarget): Promise<Member[]> =>
  fetchMembershipJson(target).then((ms) => ms.map((m) => ({ playlistId: m.id, itemId: m.item_id })));

/** Re-fetch the playlist list *and* membership after a mutation so a
 *  just-created playlist (and authoritative membership, handling duplicates)
 *  are both reflected without a second round-trip from `update`. */
const reload = (target: AddTarget) =>
  Effect.gen(function* () {
    const [playlists, members] = yield* Effect.all(
      [Effect.tryPromise(() => fetchAllPlaylists()), Effect.tryPromise(() => fetchMembers(target))],
      { concurrency: "unbounded" },
    );
    return CompletedMutation({ forTarget: target, playlists, members });
  });

export const LoadAddPanel = Command.define(
  "LoadAddPanel",
  { target: AddTarget },
  CompletedLoad,
  FailedLoad,
)(({ target }) =>
  Effect.gen(function* () {
    const [playlists, members] = yield* Effect.all(
      [Effect.tryPromise(() => fetchAllPlaylists()), Effect.tryPromise(() => fetchMembers(target))],
      { concurrency: "unbounded" },
    );
    return CompletedLoad({ forTarget: target, playlists, members });
  }).pipe(Effect.catch(() => Effect.succeed(FailedLoad({ forTarget: target })))),
);

export const AddItem = Command.define(
  "AddItem",
  { target: AddTarget, playlistId: S.Number },
  CompletedMutation,
  FailedMutation,
)(({ target, playlistId }) =>
  Effect.gen(function* () {
    const resp = yield* Effect.tryPromise(() => addPlaylistItem(playlistId, addItemBody(target)));
    if (!resp.ok) {
      const text = yield* Effect.tryPromise(() => resp.text());
      return FailedMutation({ forTarget: target, errorMessage: "Couldn't add: " + text });
    }
    return yield* reload(target);
  }).pipe(
    Effect.catch(() =>
      Effect.succeed(FailedMutation({ forTarget: target, errorMessage: "Couldn't add to playlist." })),
    ),
  ),
);

export const RemoveItem = Command.define(
  "RemoveItem",
  { target: AddTarget, playlistId: S.Number, itemId: S.Number },
  CompletedMutation,
  FailedMutation,
)(({ target, playlistId, itemId }) =>
  Effect.gen(function* () {
    const resp = yield* Effect.tryPromise(() => removePlaylistItem(playlistId, itemId));
    // 404 is treated as success (concurrent removal).
    if (!resp.ok && resp.status !== 404) {
      const text = yield* Effect.tryPromise(() => resp.text());
      return FailedMutation({ forTarget: target, errorMessage: "Couldn't remove: " + text });
    }
    return yield* reload(target);
  }).pipe(
    Effect.catch(() =>
      Effect.succeed(FailedMutation({ forTarget: target, errorMessage: "Couldn't remove from playlist." })),
    ),
  ),
);

export const CreateAndAdd = Command.define(
  "CreateAndAdd",
  { target: AddTarget, name: S.String },
  CompletedMutation,
  FailedMutation,
)(({ target, name }) =>
  Effect.gen(function* () {
    const plResp = yield* Effect.tryPromise(() => createPlaylist({ name }));
    if (!plResp.ok) {
      const text = yield* Effect.tryPromise(() => plResp.text());
      return FailedMutation({ forTarget: target, errorMessage: "Couldn't create: " + text });
    }
    const created = yield* Effect.tryPromise(() => readJson<CreatedPlaylistJson>(plResp));
    const addResp = yield* Effect.tryPromise(() => addPlaylistItem(created.id, addItemBody(target)));
    if (!addResp.ok) {
      const text = yield* Effect.tryPromise(() => addResp.text());
      return FailedMutation({ forTarget: target, errorMessage: "Couldn't add: " + text });
    }
    return yield* reload(target);
  }).pipe(
    Effect.catch(() =>
      Effect.succeed(FailedMutation({ forTarget: target, errorMessage: "Couldn't create playlist." })),
    ),
  ),
);

export const ScrollActiveIntoView = Command.define(
  "ScrollActiveIntoView",
  { rowId: RowId },
  CompletedScrollActiveIntoView,
)(({ rowId }) =>
  Dom.scrollIntoView(`#add-pl-opt-${rowId}`).pipe(
    Effect.ignore,
    Effect.as(CompletedScrollActiveIntoView()),
  ),
);

/** Focus the filter input when the panel opens. Dom.focus runs after the next
 *  render commits, so the input is in place (mirrors @foldkit/ui combobox's
 *  FocusInput). Autofocus is the wrong tool here — it only fires during HTML
 *  parsing, not for an input the runtime renders dynamically. */
export const FocusFilter = Command.define(
  "FocusFilter",
  CompletedFocusFilter,
)(Dom.focus("#add-pl-filter").pipe(Effect.ignore, Effect.as(CompletedFocusFilter())));

export const RequestClose = Command.define(
  "RequestClose",
  CompletedRequestClose,
)(Port.emit(ports.outbound.requestClose, undefined).pipe(Effect.as(CompletedRequestClose())));

export const RequestNewName = Command.define(
  "RequestNewName",
  CompletedRequestNewName,
)(
  Port.emit(ports.outbound.requestNewName, undefined).pipe(Effect.as(CompletedRequestNewName())),
);
