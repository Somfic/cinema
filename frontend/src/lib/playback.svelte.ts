import { api } from "$lib/api";
import { browserCapabilities, castCapabilities } from "$lib/capabilities";
import { downloadManager } from "$lib/downloads.svelte";
import { settings } from "$lib/settings.svelte";
import type {
	AudioTrack,
	Chapter,
	EmbeddedSubtitleTrack,
	MediaItem,
	Playback,
	StreamStats,
	SubtitleCue,
	SubtitleTrack,
	TranscodingOption,
} from "$lib/schema";

/** Where the stream is being played: its decoders decide what the server
 *  has to re-encode. */
export type PlaybackTarget = "browser" | "cast";

export interface PlaybackContext {
	item: () => MediaItem | null;
	season: () => number | null;
	episode: () => number | null;
	currentStream: () => { info_hash: string; file_idx: number } | null;
	onError?: (message: string) => void;
}

/**
 * Shared playback orchestration: subtitle loading, audio-track polling,
 * stream-stats subscriptions, the playback session lifecycle, transcoding
 * toggle, and progress saving.
 *
 * The server decides how little work gets a file playing on the target (the
 * file as is, or HLS with only the undecodable streams re-encoded). HLS
 * playlists cover the whole file, so the player seeks natively and positions
 * are positions in the file throughout.
 *
 * Inputs are passed as getter callbacks so the session reacts to the
 * surrounding route's reactive state (e.g. the details page can switch
 * `selectedStream` mid-playback) without prop-drilling.
 */
export class PlaybackSession {
	streamUrl = $state<string | null>(null);
	/** How the current stream reaches the player. */
	playback = $state<Playback | null>(null);
	/** Who decodes the stream; casting replans for the Chromecast. */
	target = $state<PlaybackTarget>("browser");

	subtitleTracks = $state<SubtitleTrack[]>([]);
	activeCues = $state<SubtitleCue[]>([]);
	activeTrackUrl = $state<string | undefined>(undefined);
	loadingSubtitles = $state(false);
	embeddedSubtitleTracks = $state<EmbeddedSubtitleTrack[]>([]);

	fileAudioTracks = $state<AudioTrack[]>([]);
	fileChapters = $state<Chapter[]>([]);
	activeAudioIdx = $state(0);
	mediaDuration = $state(0);

	hlsSessionId = $state<string | null>(null);
	/** Position to continue from when the stream is replaced mid-playback
	 *  (audio switch, transcoding change, cast handoff). The player's own
	 *  position at that moment belongs to the old stream. */
	resumeAt = $state<number | null>(null);
	transcoding = $state({ enabled: false, onlyAudio: false });

	streamStats = $state<StreamStats | null>(null);
	pieceMap = $state<number[]>([]);

	// True when a completed pretranscoding for the current stream already
	// exists for the given mode
	hasAudioPretranscoding = $derived.by(() => {
		const s = this.ctx.currentStream();
		return (
			!!s &&
			downloadManager.hasCompletedPretranscoding(s.info_hash, s.file_idx, true, this.activeAudioIdx)
		);
	});
	hasFullPretranscoding = $derived.by(() => {
		const s = this.ctx.currentStream();
		return (
			!!s &&
			downloadManager.hasCompletedPretranscoding(s.info_hash, s.file_idx, false, this.activeAudioIdx)
		);
	});

	#audioPollTimer: ReturnType<typeof setInterval> | undefined;
	#statsUnsub: (() => void) | undefined;
	#piecesUnsub: (() => void) | undefined;
	#currentlyPlaying: { info_hash: string; file_idx: number } | null = null;

	constructor(private ctx: PlaybackContext) { }

	// lifecycle

	async start(
		stream: { info_hash: string; file_idx: number },
		options?: { startAt?: number; transcoding?: TranscodingOption },
	): Promise<void> {
		await this.stopHlsSession();
		this.#resetState();
		await this.#stopStream();

		const { startAt = 0, transcoding } = options ?? {};
		this.transcoding.enabled = transcoding === "Enabled" || transcoding === "OnlyAudio";
		this.transcoding.onlyAudio = transcoding === "OnlyAudio";
		await this.#play(stream.info_hash, stream.file_idx, 0, startAt > 0 ? startAt : null);
		// #play leaves streamUrl unset when the stream was superseded or failed.
		if (!this.streamUrl) return;

		this.#pollAudioTracks(stream.info_hash, stream.file_idx);
		this.#pollStreamStats(stream.info_hash, stream.file_idx);
	}

	stop(): void {
		if (this.#audioPollTimer) {
			clearInterval(this.#audioPollTimer);
			this.#audioPollTimer = undefined;
		}
		this.#stopStream();
		this.#stopStreamStats();
		this.stopHlsSession();
		this.#resetState();
	}

	async #stopStream() {
		if (this.#currentlyPlaying) {
			try {
				await api.streams.stop(this.#currentlyPlaying.info_hash, this.#currentlyPlaying.file_idx);
			} catch {
				// do nothing
			} finally {
				this.#currentlyPlaying = null;
			}
		}
	}

	// Reset all per-stream display state. Called from both `start()` (so a
	// new stream doesn't inherit the previous one's subtitles, stats,
	// transcoding toggle, etc.) and `stop()`.
	#resetState(): void {
		this.streamUrl = null;
		this.playback = null;
		this.subtitleTracks = [];
		this.activeCues = [];
		this.activeTrackUrl = undefined;
		this.loadingSubtitles = false;
		this.embeddedSubtitleTracks = [];
		this.fileAudioTracks = [];
		this.fileChapters = [];
		this.activeAudioIdx = 0;
		this.mediaDuration = 0;
		this.transcoding.enabled = false;
		this.transcoding.onlyAudio = false;
		this.resumeAt = null;
		this.streamStats = null;
		this.pieceMap = [];
	}

	// Subtitles

	async loadSubtitles(): Promise<void> {
		const item = this.ctx.item();
		if (!item) return;
		this.loadingSubtitles = true;
		try {
			let external: SubtitleTrack[] = [];
			if (item.media_type === "movie") {
				external = await api.subtitles.movie(item.tmdb_id);
			} else {
				const s = this.ctx.season();
				const e = this.ctx.episode();
				if (s !== null && e !== null) {
					external = await api.subtitles.tv(item.tmdb_id, s, e);
				}
			}
			// Preserve any embedded tracks that `#pollAudioTracks` may have
			// already prepended - assigning the external list directly would
			// otherwise drop them when audio polling resolves first.
			const embedded = this.subtitleTracks.filter((t) =>
				t.id.startsWith("embedded:"),
			);
			this.subtitleTracks = [...embedded, ...external];
			if (!this.activeTrackUrl && this.subtitleTracks.length > 0) {
				await this.selectSubtitleTrack(this.subtitleTracks[0]);
			}
		} catch {
			// Subtitles are optional.
		} finally {
			this.loadingSubtitles = false;
		}
	}

	async selectSubtitleTrack(track: SubtitleTrack): Promise<void> {
		this.loadingSubtitles = true;
		this.activeTrackUrl = track.url;
		try {
			const stream = this.ctx.currentStream();
			if (track.id.startsWith("embedded:") && stream) {
				// Embedded cues are extracted on demand over RPC, keyed by the
				// subtitle track index encoded in the track id.
				const streamIndex = Number(track.id.slice("embedded:".length));
				this.activeCues = await api.streams.embeddedSubtitles(
					stream.info_hash,
					stream.file_idx,
					streamIndex,
				);
			} else {
				this.activeCues = await api.subtitles.cues(track.url);
			}
		} catch {
			this.activeCues = [];
		} finally {
			this.loadingSubtitles = false;
		}
	}

	disableSubtitles(): void {
		this.activeCues = [];
		this.activeTrackUrl = undefined;
	}

	// Audio + structure polling

	#pollAudioTracks(hash: string, idx: number): void {
		if (this.#audioPollTimer) clearInterval(this.#audioPollTimer);

		const check = async () => {
			try {
				const data = await api.streams.audioTracks(hash, idx);
				// Discard results from a superseded stream: an episode/source
				// switch may have happened while this request was in flight,
				// and applying its data would write the previous file's
				// chapters/audio tracks/subtitles back over the new one.
				const cur = this.ctx.currentStream();
				if (!cur || cur.info_hash !== hash || cur.file_idx !== idx) return;
				const tracks = (data.tracks ?? []) as AudioTrack[];
				const subs = (data.subtitles ?? []) as EmbeddedSubtitleTrack[];
				if (data.duration) this.mediaDuration = data.duration;
				if (data.chapters?.length) this.fileChapters = data.chapters as Chapter[];
				if (tracks.length === 0) return; // keep polling
				if (tracks.length > 1) this.fileAudioTracks = tracks;
				if (subs.length > 0 && this.embeddedSubtitleTracks.length === 0) {
					this.embeddedSubtitleTracks = subs;
					// Prepend embedded tracks to the subtitle list. The url is
					// a synthetic id; embedded cues are fetched over RPC.
					const embedded: SubtitleTrack[] = subs.map((s) => ({
						id: `embedded:${s.stream_index}`,
						language: s.language ?? "und",
						url: `embedded:${s.stream_index}`,
						score: 1000,
					}));
					this.subtitleTracks = [...embedded, ...this.subtitleTracks];
					if (!this.activeTrackUrl && embedded.length > 0) {
						this.selectSubtitleTrack(embedded[0]);
					}
				}
				// Stop polling once we have a duration; keep going if it
				// hasn't resolved yet (e.g. an mp4 whose moov atom isn't
				// downloaded).
				if (this.mediaDuration > 0) {
					clearInterval(this.#audioPollTimer);
					this.#audioPollTimer = undefined;
				}
			} catch { }
		};

		check();
		this.#audioPollTimer = setInterval(check, 10_000);
	}

	// Stats subscriptions

	#pollStreamStats(hash: string, idx: number): void {
		this.#stopStreamStats();
		this.streamStats = null;
		this.pieceMap = [];

		this.#statsUnsub = api.streamsEvents.onStats((p) => {
			if (p.info_hash !== hash) return;
			this.streamStats = {
				progress_bytes: p.progress_bytes,
				total_bytes: p.total_bytes,
				download_speed_mbps: p.download_speed_mbps,
				peers: p.peers,
				finished: p.finished,
			};
		});

		this.#piecesUnsub = api.streamsEvents.onPieces((p) => {
			if (p.info_hash !== hash || p.file_idx !== idx) return;
			this.pieceMap = p.pieces;
		});
	}

	#stopStreamStats(): void {
		if (this.#statsUnsub) {
			this.#statsUnsub();
			this.#statsUnsub = undefined;
		}
		if (this.#piecesUnsub) {
			this.#piecesUnsub();
			this.#piecesUnsub = undefined;
		}
	}

	// Audio switching / transcoding / cast handoff
	//
	// Each replaces the stream and picks up where playback was.

	async switchAudio(idx: number, currentTime: number): Promise<void> {
		const stream = this.ctx.currentStream();
		if (!stream) return;
		this.activeAudioIdx = idx;
		await this.#play(stream.info_hash, stream.file_idx, idx, currentTime);
	}

	async toggleTranscoding(
		enabled: boolean,
		onlyAudio: boolean,
		currentTime: number,
	): Promise<void> {
		const stream = this.ctx.currentStream();
		this.transcoding.enabled = enabled;
		this.transcoding.onlyAudio = enabled && onlyAudio;
		if (!stream) return;
		await this.#play(stream.info_hash, stream.file_idx, this.activeAudioIdx, currentTime);
	}

	/** Turns HDR on or off for HDR sources; off tone maps to SDR. The
	 *  choice is remembered for later streams. */
	async setHdr(on: boolean, currentTime: number): Promise<void> {
		settings.setKeepHdr(on);
		const stream = this.ctx.currentStream();
		if (!stream || !this.playback?.source_hdr) return;
		await this.#play(stream.info_hash, stream.file_idx, this.activeAudioIdx, currentTime);
	}

	/** Moves playback to another decoder (the browser, or a Chromecast):
	 *  what needs re-encoding depends on who decodes it. */
	async retarget(target: PlaybackTarget, currentTime: number): Promise<void> {
		if (this.target === target) return;
		this.target = target;
		const stream = this.ctx.currentStream();
		if (!stream || !this.streamUrl) return;
		await this.#play(stream.info_hash, stream.file_idx, this.activeAudioIdx, currentTime);
	}

	#mode(): TranscodingOption {
		if (!this.transcoding.enabled) return "Disabled";
		return this.transcoding.onlyAudio ? "OnlyAudio" : "Enabled";
	}

	async #play(
		hash: string,
		idx: number,
		audioIdx: number,
		resumeAt: number | null,
	): Promise<void> {
		await this.stopHlsSession();
		this.streamUrl = null;
		if (this.#currentlyPlaying && (this.#currentlyPlaying.info_hash !== hash || this.#currentlyPlaying.file_idx !== idx)) {
			await this.#stopStream();
		}
		const client =
			this.target === "cast" ? castCapabilities(settings.cast) : browserCapabilities();
		try {
			const playback = await api.streams.play(
				hash,
				idx,
				audioIdx,
				client,
				this.#mode(),
				settings.keepHdr,
			);
			// The user may have switched streams while this was in flight.
			// Drop the orphan session so it doesn't leak into the new stream.
			const cur = this.ctx.currentStream();
			if (!cur || cur.info_hash !== hash || cur.file_idx !== idx) {
				if (playback.session_id) api.hls.stop(playback.session_id).catch(() => { });
				return;
			}
			this.playback = playback;
			this.hlsSessionId = playback.session_id;
			this.resumeAt = resumeAt;
			if (playback.duration) this.mediaDuration = playback.duration;
			this.streamUrl = playback.url;
			this.#currentlyPlaying = { info_hash: hash, file_idx: idx };
		} catch (e: unknown) {
			const msg = e instanceof Error ? e.message : String(e);
			this.ctx.onError?.(msg);
		}
	}

	// Resolves once the server has torn the session down, so the replaced
	// session's pipeline stops competing with the new one.
	async stopHlsSession(): Promise<void> {
		const sessionId = this.hlsSessionId;
		if (!sessionId) return;
		this.hlsSessionId = null;
		await api.hls.stop(sessionId).catch(() => { });
	}

	// Progress

	saveProgress(playerTime: number, playerDuration: number): void {
		const item = this.ctx.item();
		if (!item || playerTime <= 0) return;
		const stream = this.ctx.currentStream();
		if (!stream) return;
		const s = this.ctx.season();
		const e = this.ctx.episode();
		if (item.media_type === "tv" && (s === null || e === null)) return;
		api.watch
			.record({
				media_type: item.media_type,
				tmdb_id: item.tmdb_id,
				season: s ?? null,
				episode: e ?? null,
				info_hash: stream.info_hash,
				file_idx: stream.file_idx,
				progress: playerTime,
				duration: playerDuration,
				transcoding: this.transcoding.onlyAudio ? "OnlyAudio" : this.transcoding.enabled ? "Enabled" : "Disabled",
			})
			.catch(() => { });
	}
}
