// Bridges a `PlaybackSession` to a Chromecast session: replans the stream for
// the receiver's decoders, pushes it there, and keeps captions in sync.
//
// Both players mount this — the standalone play route and the in-page player
// the details page opens — so casting behaves the same wherever playback
// started from.

import { untrack } from "svelte";
import { api } from "$lib/api";
import { cast, castAbsoluteUrl, type CastTextTrack } from "$lib/cast.svelte";
import type { PlaybackSession } from "$lib/playback.svelte";

export interface CastPlaybackContext {
	session: PlaybackSession;
	/** The stream being played, or null when the player is idle. */
	stream: () => { info_hash: string; file_idx: number } | null;
	/** Live playback position, used as the receiver's start point. */
	currentTime: () => number;
	title: () => string | undefined;
	subtitle: () => string | undefined;
}

/**
 * Wires cast playback for the calling component. Must be called during
 * component initialisation; the effects it creates are torn down with the
 * component.
 */
export function castPlayback(ctx: CastPlaybackContext): void {
	const { session } = ctx;

	// Caption tracks the receiver can sideload: the same list the inline
	// player shows, re-pointed at the server's WebVTT renderings.
	const textTracks = $derived.by<CastTextTrack[]>(() => {
		const stream = ctx.stream();
		if (!stream) return [];
		return session.subtitleTracks.map((track, i) => ({
			// Cast track ids are numeric; index is stable for a given list.
			id: i + 1,
			url: castAbsoluteUrl(
				track.id.startsWith("embedded:")
					? api.urls.embeddedSubtitles(
							stream.info_hash,
							stream.file_idx,
							Number(track.id.slice("embedded:".length)),
						)
					: api.urls.externalSubtitles(track.url),
			),
			label: track.language,
			language: track.language,
		}));
	});

	// What the receiver can decode differs from the browser, so connecting
	// replans the stream for the Chromecast (and disconnecting replans it
	// for the browser). Usually the video is copied either way and at most
	// the audio is re-encoded.
	let castTarget = false;
	$effect(() => {
		const connected = cast.connected;
		if (connected === castTarget) return;
		castTarget = connected;
		untrack(() => {
			session.retarget(connected ? "cast" : "browser", ctx.currentTime());
		});
	});

	let loadedUrl: string | null = null;
	let loadedTrackCount = 0;

	// Blank the TV the moment a session connects. A live transcode can take
	// tens of seconds to produce its first segment, and until the receiver has
	// been told to load something it shows nothing but its own ambient
	// backdrop. Replaced by the real stream below.
	let postedPlaceholder = false;
	$effect(() => {
		if (!cast.connected) {
			postedPlaceholder = false;
			return;
		}
		if (postedPlaceholder || loadedUrl) return;
		postedPlaceholder = true;
		cast.loadPlaceholder().catch(() => { });
	});

	// Push media to the receiver whenever the thing being played changes — a
	// new stream (source switch, audio switch, transcoding change) or a fresh
	// cast session. The last-loaded url keeps an unrelated state change from
	// reloading the receiver mid-playback.
	$effect(() => {
		if (!cast.connected) {
			loadedUrl = null;
			return;
		}
		// Wait for the stream planned for the receiver.
		if (session.target !== "cast") return;
		const url = session.streamUrl;
		const playback = session.playback;
		if (!url || !playback) return;
		const tracks = textTracks;
		// Subtitles resolve a moment after playback starts, so a cast that
		// began with none reloads once to pick them up — captions can't be
		// added to media the receiver has already loaded.
		if (url === loadedUrl && tracks.length === loadedTrackCount) return;

		// Positions are positions in the file for every stream. A reload of
		// the same url (captions arriving) resumes at the live position; a new
		// stream starts where the one it replaced was.
		const startAt = untrack(() =>
			url === loadedUrl ? ctx.currentTime() : (session.resumeAt ?? ctx.currentTime()),
		);
		const activeIndex = session.subtitleTracks.findIndex(
			(t) => t.url === session.activeTrackUrl,
		);
		loadedUrl = url;
		loadedTrackCount = tracks.length;
		const hls = playback.kind === "Hls";
		cast
			.load({
				url: castAbsoluteUrl(url),
				contentType: hls ? "application/x-mpegurl" : "video/mp4",
				hls,
				title: ctx.title(),
				subtitle: ctx.subtitle(),
				currentTime: Math.max(0, startAt),
				tracks,
				activeTrackId: activeIndex >= 0 ? tracks[activeIndex]?.id : null,
			})
			.catch(() => {
				// `cast.error` carries the reason; allow a retry on the next change.
				loadedUrl = null;
				loadedTrackCount = 0;
			});
	});

	// Mirror subtitle selection onto the receiver once media is loaded.
	$effect(() => {
		if (!cast.connected || !cast.mediaLoaded) return;
		const activeIndex = session.subtitleTracks.findIndex(
			(t) => t.url === session.activeTrackUrl,
		);
		const id = activeIndex >= 0 ? textTracks[activeIndex]?.id : null;
		if (id != null && !cast.hasTextTrack(id)) return;
		cast.setTextTrack(session.activeCues.length > 0 ? (id ?? null) : null);
	});
}
