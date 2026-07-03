import { api } from "$lib/api";
import type {
	AudioTrack,
	Chapter,
	EmbeddedSubtitleTrack,
	MediaItem,
	StreamStats,
	SubtitleCue,
	SubtitleTrack,
	TranscodingOption,
} from "$lib/schema";

export interface PlaybackContext {
	item: () => MediaItem | null;
	season: () => number | null;
	episode: () => number | null;
	currentStream: () => { info_hash: string; file_idx: number } | null;
	onError?: (message: string) => void;
}

/**
 * Shared playback orchestration: subtitle loading, audio-track polling,
 * stream-stats subscriptions, HLS remux lifecycle, transcoding toggle, and
 * progress saving.
 *
 * Inputs are passed as getter callbacks so the session reacts to the
 * surrounding route's reactive state (e.g. the details page can switch
 * `selectedStream` mid-playback) without prop-drilling.
 */
export class PlaybackSession {
	streamUrl = $state<string | null>(null);
	playingLocal = $state(false);

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
	transcoding = $state({ enabled: false, onlyAudio: false });

	streamStats = $state<StreamStats | null>(null);
	pieceMap = $state<number[]>([]);

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
		this.stopHlsSession();
		this.#resetState();
		await this.#stopStream();

		const { startAt = 0, transcoding } = options ?? {};

		if (transcoding === "Enabled" || transcoding === "OnlyAudio") {
			const onlyAudio = transcoding === "OnlyAudio";
			this.transcoding.enabled = true;
			this.transcoding.onlyAudio = onlyAudio;
			await this.#startHlsRemux(stream.info_hash, stream.file_idx, 0, onlyAudio, startAt);
			// #startHlsRemux clears hlsSessionId when the stream was superseded or errored.
			if (!this.hlsSessionId) return;
		} else {
			const result = await api.streams.start(stream.info_hash, stream.file_idx);
			// Discard if the route switched streams while we awaited.
			const cur = this.ctx.currentStream();
			if (
				!cur ||
				cur.info_hash !== stream.info_hash ||
				cur.file_idx !== stream.file_idx
			)
				return;
			this.streamUrl = result.url;
			this.playingLocal = result.local;
			this.#currentlyPlaying = stream;
		}

		this.#pollAudioTracks(stream.info_hash, stream.file_idx);
		if (!this.playingLocal)
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
		this.playingLocal = false;
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
				// ffmpeg stream index encoded in the track id.
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

	// Audio switching / seeking / transcoding

	async switchAudio(idx: number, currentTime: number): Promise<void> {
		const stream = this.ctx.currentStream();
		if (!stream) return;
		this.activeAudioIdx = idx;
		await this.#startHlsRemux(
			stream.info_hash,
			stream.file_idx,
			idx,
			this.transcoding.onlyAudio,
			currentTime,
		);
	}

	// Seek beyond the transcoded window: restart the HLS transcode at the
	// target time. `-ss`/`-copyts` keep currentTime aligned to the absolute
	// timeline, so the player resumes at the sought position once the new
	// playlist loads.
	async seekRestart(time: number): Promise<void> {
		const stream = this.ctx.currentStream();
		if (!stream) return;
		await this.#startHlsRemux(
			stream.info_hash,
			stream.file_idx,
			this.activeAudioIdx,
			this.transcoding.onlyAudio,
			time,
		);
	}

	async toggleTranscoding(
		enabled: boolean,
		onlyAudio: boolean,
		currentTime: number,
	): Promise<void> {
		const stream = this.ctx.currentStream();
		if (!stream) return;
		if (enabled) {
			await this.#startHlsRemux(
				stream.info_hash,
				stream.file_idx,
				this.activeAudioIdx,
				onlyAudio,
				currentTime,
			);
		} else {
			this.stopHlsSession();
			const result = await api.streams.start(stream.info_hash, stream.file_idx);
			this.streamUrl = result.url;
		}
	}

	async #startHlsRemux(
		hash: string,
		idx: number,
		audioIdx: number,
		onlyAudio: boolean,
		startAt = 0,
	): Promise<void> {
		this.stopHlsSession();
		this.streamUrl = null;
		if (this.#currentlyPlaying && (this.#currentlyPlaying.info_hash !== hash || this.#currentlyPlaying.file_idx !== idx)) {
			await this.#stopStream();
		}
		try {
			const session = await api.streams.remux(
				hash,
				idx,
				audioIdx,
				startAt,
				onlyAudio,
			);
			// The remux call blocks until the first segment exists; the user
			// may have switched streams in the meantime. Drop the orphan
			// session so its playlist doesn't leak into the new stream.
			const cur = this.ctx.currentStream();
			if (!cur || cur.info_hash !== hash || cur.file_idx !== idx) {
				api.hls.stop(session.session_id).catch(() => { });
				return;
			}
			this.hlsSessionId = session.session_id;
			this.streamUrl = session.playlist_url;
			this.#currentlyPlaying = {
				info_hash: hash,
				file_idx: idx
			}
		} catch (e: unknown) {
			const msg = e instanceof Error ? e.message : String(e);
			this.ctx.onError?.(msg);
		}
	}

	stopHlsSession(): void {
		if (this.hlsSessionId) {
			api.hls.stop(this.hlsSessionId).catch(() => { });
			this.hlsSessionId = null;
		}
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
