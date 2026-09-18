// Bridges a `PlaybackSession` to a Chromecast session: forces the stream into
// a form a receiver can play, pushes it there, and keeps captions in sync.
//
// Both players mount this — the standalone play route and the in-page player
// the details page opens — so casting behaves the same wherever playback
// started from.

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

	// A receiver can't play the raw container, so casting always runs off an
	// HLS session — but it doesn't need the video re-encoded. Audio-only
	// transcode copies the video stream through and just normalises audio to
	// stereo AAC, which is nearly free and keeps the source quality intact.
	//
	// Applied once when a session connects; a later choice in the player's
	// transcoding menu is the user's, so it isn't overridden. If a particular
	// file won't play on the receiver (a codec it can't decode), switching to
	// "Audio + video" there re-encodes it.
	let appliedCastTranscoding = false;
	$effect(() => {
		if (!cast.connected) {
			appliedCastTranscoding = false;
			return;
		}
		if (appliedCastTranscoding || !session.streamUrl) return;
		appliedCastTranscoding = true;
		if (session.transcoding.enabled && session.transcoding.onlyAudio) return;
		session.transcoding.enabled = true;
		session.transcoding.onlyAudio = true;
		session.toggleTranscoding(true, true, ctx.currentTime());
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
	// new playlist (source switch, audio switch, seek-restart) or a fresh cast
	// session. The last-loaded url keeps an unrelated state change from
	// reloading the receiver mid-playback.
	$effect(() => {
		if (!cast.connected) {
			loadedUrl = null;
			return;
		}
		const url = session.streamUrl;
		if (!url || !session.hlsSessionId) return;
		const tracks = textTracks;
		// Subtitles resolve a moment after playback starts, so a cast that
		// began with none reloads once to pick them up — captions can't be
		// added to media the receiver has already loaded.
		if (url === loadedUrl && tracks.length === loadedTrackCount) return;

		// Where the receiver should pick up. A reload that keeps the same url
		// (captions arriving) resumes at the live position; a new url is a new
		// HLS session, and then the live position is the *old* session's — the
		// place a seek just moved away from. Loading there is what made a seek
		// snap back to where playback already was, so a seek-restart uses its
		// target, and any other new session its timeline origin (clamped up to
		// the live position, for a session that started before the receiver
		// joined).
		const resumeAt =
			url === loadedUrl
				? ctx.currentTime()
				: (session.hlsSeekTarget ??
					Math.max(ctx.currentTime(), session.hlsStartAt));
		// The session's playlist runs from zero; `resumeAt` is a position in
		// the file, so drop the session's origin back off it.
		const startAt = Math.max(0, resumeAt - session.hlsStartAt);
		const activeIndex = session.subtitleTracks.findIndex(
			(t) => t.url === session.activeTrackUrl,
		);
		loadedUrl = url;
		loadedTrackCount = tracks.length;
		cast
			.load({
				url: castAbsoluteUrl(url),
				contentType: "application/x-mpegurl",
				title: ctx.title(),
				subtitle: ctx.subtitle(),
				currentTime: startAt,
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
