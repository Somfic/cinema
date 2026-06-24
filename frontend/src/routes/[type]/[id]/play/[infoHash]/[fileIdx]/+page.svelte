<script lang="ts">
	import { onDestroy } from "svelte";
	import { page } from "$app/state";
	import { goto } from "$app/navigation";
	import {
		type MediaItem,
		type SubtitleTrack,
		type SubtitleCue,
		type Stream,
		type Chapter,
		type AudioTrack,
		type StreamStats,
	} from "$lib/schema";
	import { api } from "$lib/api";
	import { getDetails, imageUrl, playStream } from "$lib/utils";
	import VideoPlayer from "$lib/components/VideoPlayer.svelte";
	import { remote, type PlayerControls } from "$lib/remote.svelte";
	import type { MediaType } from "$lib/schema/tmdb";

	const BROWSER_SAFE_AUDIO = new Set(["aac", "mp3", "opus", "vorbis", "flac"]);

	let item = $state<MediaItem | null>(null);
	let streamUrl = $state<string | null>(null);
	let subtitleTracks = $state<SubtitleTrack[]>([]);
	let activeCues = $state<SubtitleCue[]>([]);
	let activeTrackUrl = $state<string | undefined>(undefined);
	let loadingSubtitles = $state(false);
	let error = $state<string | null>(null);
	let fileAudioTracks = $state<AudioTrack[]>([]);
	let fileChapters = $state<Chapter[]>([]);
	let activeAudioIdx = $state(0);
	let mediaDuration = $state(0);
	let hlsSessionId = $state<string | null>(null);
	// True while a remux request is in flight. The backend blocks until the
	// first HLS segment exists (up to the startup timeout), so without this guard
	// the audio-track poll can fire a second `startHlsRemux` before the first
	// resolves, spawning duplicate ffmpeg sessions for the same file.
	let hlsStarting = false;

	let streamStats = $state<StreamStats | null>(null);
	let pieceMap = $state<number[]>([]);
	let statsUnsub: (() => void) | undefined;
	let piecesUnsub: (() => void) | undefined;
	let transcoding = $state({ enabled: false, onlyAudio: false });
	// Alternative streams/sources for this title, so a paired remote can offer
	// the same source menu the inline player has.
	let streams = $state<Stream[]>([]);
	// Resume position (seconds) applied once on first load.
	let startTime = $state(0);

	const backdropUrls = $derived(
		item?.backdrops?.map((b) => imageUrl(b, "original")) ?? [],
	);

	// Backdrop shown behind the player while it loads — the episode still for a
	// TV episode, otherwise the movie/show backdrop.
	const playerBackdrop = $derived.by(() => {
		if (mediaType === "tv" && season !== null && episode !== null) {
			const still = item?.seasons
				?.find((s) => s.season_number === season)
				?.episodes?.find((e) => e.episode_number === episode)
				?.stills?.[0];
			if (still) return imageUrl(still, "original");
		}
		return backdropUrls[0];
	});

	const mediaType = $derived(page.params.type as MediaType);
	const mediaId = $derived(Number(page.params.id));
	const infoHash = $derived(page.params.infoHash);
	const fileIdx = $derived(Number(page.params.fileIdx));

	// Direct (raw, untranscoded) stream URL for the "open in external player"
	// control — a desktop player handles any codec, so it's offered regardless
	// of whether the browser can play the file inline.
	const externalUrl = $derived(api.urls.stream(infoHash as string, fileIdx));

	// Parse season/episode from query params for TV
	const season = $derived(
		page.url.searchParams.get("s")
			? Number(page.url.searchParams.get("s"))
			: null,
	);
	const episode = $derived(
		page.url.searchParams.get("e")
			? Number(page.url.searchParams.get("e"))
			: null,
	);

	const episodeName = $derived(() => {
		if (!item || mediaType !== "tv" || season === null || episode === null)
			return null;
		const s = item.seasons?.find((s) => s.season_number === season);
		return s?.episodes?.find((e) => e.episode_number === episode)?.name ?? null;
	});

	const playerTitle = $derived(
		mediaType === "tv" && item
			? (episodeName() ?? `Episode ${episode}`)
			: item?.title,
	);
	const playerTopline = $derived(
		mediaType === "tv" && item && season !== null && episode !== null
			? `S${season} E${episode} · ${item.title}`
			: undefined,
	);

	function pollStreamStats(hash: string) {
		if (statsUnsub) statsUnsub();
		if (piecesUnsub) piecesUnsub();
		streamStats = null;
		pieceMap = [];

		statsUnsub = api.streamsEvents.onStats((p) => {
			if (p.info_hash !== hash) return;
			streamStats = {
				progress_bytes: p.progress_bytes,
				total_bytes: p.total_bytes,
				download_speed_mbps: p.download_speed_mbps,
				peers: p.peers,
				finished: p.finished,
			};
		});

		piecesUnsub = api.streamsEvents.onPieces((p) => {
			if (p.info_hash !== hash || p.file_idx !== fileIdx) return;
			pieceMap = p.pieces;
		});
	}

	$effect(() => {
		const detailsPromise = getDetails(mediaType, mediaId)
			.then((res) => {
				item = res;
			})
			.catch((e) => {
				error = e.message;
			});

		const streamPromise = playStream(infoHash as string, fileIdx as number)
			.then(async (result) => {
				streamUrl = result.url;
				activeAudioIdx = 0;
				pollAudioTracks(infoHash as string, fileIdx as number);
				pollStreamStats(infoHash as string);
			})
			.catch((e) => {
				error = e.message;
			});

		Promise.all([detailsPromise, streamPromise]).then(() => {
			loadSubtitleTracks();
		});
	});

	// Fetch the alternative streams for this title (for the remote's source menu).
	$effect(() => {
		(async () => {
			try {
				if (mediaType === "movie") {
					streams = await api.streams.movie(mediaId);
				} else if (season !== null && episode !== null) {
					streams = await api.streams.tv(mediaId, season, episode);
				}
			} catch {
				streams = [];
			}
		})();
	});

	// ── Resume + progress ──
	// Resume from the last watched position for this title/episode.
	$effect(() => {
		const type = mediaType;
		const id = mediaId;
		const s = season;
		const e = episode;
		api.watch
			.history()
			.then((items) => {
				const entry = items.find(
					(w) =>
						w.media_type === type &&
						w.tmdb_id === id &&
						(type === "movie" ||
							(w.season === s && w.episode === e)) &&
						w.progress > 0,
				);
				if (
					entry &&
					entry.duration > 0 &&
					entry.progress < entry.duration - 30
				) {
					startTime = entry.progress;
				}
			})
			.catch(() => {});
	});

	// Once playback reaches the resume point, stop re-applying it so a later
	// transcode/source switch resumes from the live position instead.
	$effect(() => {
		if (startTime > 0 && playerTime >= startTime - 0.5) startTime = 0;
	});

	function saveProgress() {
		if (!item || playerTime <= 0) return;
		if (mediaType === "tv" && (season === null || episode === null)) return;
		api.watch
			.record({
				media_type: mediaType,
				tmdb_id: mediaId,
				title: item.title,
				poster_path: item.poster_path ?? null,
				season: season ?? null,
				episode: episode ?? null,
				info_hash: infoHash as string,
				file_idx: fileIdx,
				progress: playerTime,
				duration: playerDuration,
			})
			.catch(() => {});
	}

	// Save periodically while playing.
	$effect(() => {
		if (!streamUrl) return;
		const interval = setInterval(saveProgress, 30_000);
		return () => clearInterval(interval);
	});

	// Switch source/quality by re-navigating the play route to the new stream.
	function selectStream(hash: string) {
		const stream = streams.find((s) => s.info_hash === hash);
		if (!stream) return;
		const qs = new URLSearchParams();
		if (season !== null) qs.set("s", String(season));
		if (episode !== null) qs.set("e", String(episode));
		const q = qs.toString();
		goto(
			`/${mediaType}/${mediaId}/play/${stream.info_hash}/${stream.file_idx}${q ? `?${q}` : ""}`,
		);
	}

	async function loadSubtitleTracks() {
		if (!item) return;
		loadingSubtitles = true;
		try {
			if (mediaType === "movie") {
				subtitleTracks = await api.subtitles.movie(item.id);
			} else if (season !== null && episode !== null) {
				subtitleTracks = await api.subtitles.tv(
					item.id,
					season,
					episode,
				);
			}

			if (subtitleTracks.length > 0) {
				await selectSubtitleTrack(subtitleTracks[0]);
			}
		} catch {
			// Subtitles are optional
		} finally {
			loadingSubtitles = false;
		}
	}

	async function selectSubtitleTrack(track: SubtitleTrack) {
		loadingSubtitles = true;
		activeTrackUrl = track.url;
		try {
			activeCues = await api.subtitles.cues(track.url);
		} catch {
			activeCues = [];
		} finally {
			loadingSubtitles = false;
		}
	}

	function disableSubtitles() {
		activeCues = [];
		activeTrackUrl = undefined;
	}

	let audioPollTimer: ReturnType<typeof setInterval> | undefined;

	function pollAudioTracks(hash: string, idx: number) {
		if (audioPollTimer) clearInterval(audioPollTimer);

		const check = async () => {
			try {
				const data = await api.streams.audioTracks(hash, idx);
				const tracks: AudioTrack[] = data.tracks ?? [];
				if (data.duration) mediaDuration = data.duration;
				if (data.chapters?.length) fileChapters = data.chapters;
				if (tracks.length === 0) return; // keep polling until tracks appear
				if (tracks.length > 1) fileAudioTracks = tracks;
				if (
					!hlsSessionId &&
					!hlsStarting &&
					tracks[0] &&
					!BROWSER_SAFE_AUDIO.has(tracks[0].codec)
				) {
					fileAudioTracks = tracks;
					transcoding.enabled = true;
					transcoding.onlyAudio = true;
					startHlsRemux(hash, idx, 0, transcoding.onlyAudio);
				}
				// Stop once we also have a duration; keep polling if it hasn't
				// resolved yet (e.g. an mp4 whose moov atom isn't downloaded).
				if (mediaDuration > 0) {
					clearInterval(audioPollTimer);
					audioPollTimer = undefined;
				}
			} catch {}
		};

		check();
		audioPollTimer = setInterval(check, 10_000);
	}

	let playerTime = $state(0);
	let playerPaused = $state(true);
	let playerVolume = $state(1);
	let playerDuration = $state(0);
	let playerBuffered = $state(0);
	let playerSubtitleOffset = $state(-0.25);
	let playerLoading = $state(true);
	// Loose type: the bound instance exposes the VideoPlayer's `export function`s.
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	let playerRef = $state<any>(undefined);

	// Integer-second tick: throttles the TV→remote state feed to ~1 Hz instead
	// of firing on every `timeupdate`.
	const tvSecond = $derived(Math.floor(playerTime));

	// TV mode: register the player so a paired remote can drive it, and stream
	// now-playing state back to the remote.
	$effect(() => {
		if (remote.mode !== "tv" || !playerRef) return;
		const controls: PlayerControls = {
			play: () => playerRef.play(),
			pause: () => playerRef.pause(),
			togglePlay: () => playerRef.togglePlay(),
			seekTo: (t) => playerRef.seekTo(t),
			seekBy: (d) => playerRef.seekBy(d),
			setVolume: (v) => playerRef.setVolumeValue(v),
			toggleMute: () => playerRef.toggleMute(),
			toggleFullscreen: () => playerRef.toggleFullscreen(),
			selectStream: (hash) => selectStream(hash),
			selectAudio: (id) => switchAudio(id),
			selectSubtitle: (url) => {
				const track = subtitleTracks.find((t) => t.url === url);
				if (track) selectSubtitleTrack(track);
			},
			subtitleOff: () => disableSubtitles(),
			setSubtitleOffset: (delta) => {
				playerSubtitleOffset += delta;
			},
			setTranscoding: (enabled, onlyAudio) => {
				// Update the state the player binds to (the inline menu does this
				// via binding; the command path must do it explicitly) so the
				// published TvState reflects the toggle and the remote doesn't
				// snap back.
				transcoding.enabled = enabled;
				transcoding.onlyAudio = onlyAudio;
				toggleTranscoding(enabled, onlyAudio);
			},
			exit: () => close(),
		};
		remote.registerPlayer(controls);
		return () => remote.clearPlayer();
	});

	$effect(() => {
		if (remote.mode !== "tv") return;
		remote.publishState({
			title: playerTitle ?? undefined,
			titleImage: item?.logo_path
				? imageUrl(item.logo_path, "original")
				: undefined,
			topline: playerTopline ?? undefined,
			poster: backdropUrls[0],
			currentTime: tvSecond,
			duration: playerDuration,
			buffered: playerBuffered,
			loading: playerLoading,
			paused: playerPaused,
			volume: playerVolume,
			muted: playerVolume === 0,
			playing: !!streamUrl,
			streams: streams.map((s) => ({
				info_hash: s.info_hash,
				resolution: s.resolution,
				source: s.source,
				codec: s.codec,
				audio: s.audio,
				source_type: s.source_type,
				size_display: s.size_display,
			})),
			activeStreamHash: infoHash,
			audioTracks: fileAudioTracks.map((t) => ({
				id: t.stream_index,
				name: t.name,
				lang: t.language ?? undefined,
			})),
			activeAudioTrack: activeAudioIdx,
			subtitleTracks: subtitleTracks.map((t) => ({
				id: t.id,
				language: t.language,
				url: t.url,
			})),
			activeTrackUrl,
			subtitlesActive: activeCues.length > 0,
			subtitleOffset: playerSubtitleOffset,
			transcoding: {
				enabled: transcoding.enabled,
				onlyAudio: transcoding.onlyAudio,
			},
			streamStats,
			pieceMap,
		});
	});

	// Leaving playback: tell the paired remote the TV is idle again so it
	// resumes mirroring the phone's browsing (a stale playing:true would
	// otherwise keep navigation suppressed).
	onDestroy(() => {
		saveProgress();
		if (remote.mode === "tv") {
			remote.publishState({
				currentTime: 0,
				duration: 0,
				paused: true,
				volume: playerVolume,
				muted: false,
				playing: false,
			});
		}
		remote.clearPlayer();
	});

	async function switchAudio(idx: number) {
		activeAudioIdx = idx;
		await startHlsRemux(
			infoHash as string,
			fileIdx as number,
			idx,
			transcoding.onlyAudio,
			playerTime,
		);
	}

	// Seek beyond the transcoded window: restart the HLS transcode at the target
	// time. `-ss`/`-copyts` keep currentTime aligned to the absolute timeline,
	// so the player resumes at the sought position once the new playlist loads.
	async function seekRestart(time: number) {
		await startHlsRemux(
			infoHash as string,
			fileIdx as number,
			activeAudioIdx,
			transcoding.onlyAudio,
			time,
		);
	}

	async function toggleTranscoding(enabled: boolean, onlyAudio: boolean) {
		if (enabled) {
			await startHlsRemux(
				infoHash as string,
				fileIdx as number,
				activeAudioIdx,
				onlyAudio,
				playerTime,
			);
		} else {
			stopHlsSession();
			const result = await playStream(infoHash as string, fileIdx as number);
			streamUrl = result.url;
		}
	}

	async function startHlsRemux(
		hash: string,
		idx: number,
		audioIdx: number,
		onlyAudio: boolean,
		startAt = 0,
	) {
		stopHlsSession();
		streamUrl = null;
		hlsStarting = true;
		try {
			const session = await api.streams.remux(
				hash,
				idx,
				audioIdx,
				startAt,
				onlyAudio,
			);
			hlsSessionId = session.session_id;
			streamUrl = session.playlist_url;
		} catch (e: any) {
			error = e.message;
		} finally {
			hlsStarting = false;
		}
	}

	function stopHlsSession() {
		if (hlsSessionId) {
			api.hls.stop(hlsSessionId).catch(() => {});
			hlsSessionId = null;
		}
	}

	function close() {
		if (statsUnsub) {
			statsUnsub();
			statsUnsub = undefined;
		}
		if (piecesUnsub) {
			piecesUnsub();
			piecesUnsub = undefined;
		}
		stopHlsSession();
		goto(`/${mediaType}/${mediaId}`);
	}
</script>

<div class="player-page">
	{#if error}
		<div class="error-overlay">
			<p>{error}</p>
			<button onclick={close}>Go back</button>
		</div>
	{:else if streamUrl}
		<VideoPlayer
			src={streamUrl}
			subtitles={activeCues}
			title={playerTitle}
			topline={playerTopline}
			titleImage={item?.logo_path
				? imageUrl(item.logo_path, "original")
				: undefined}
			audioTracks={fileAudioTracks.map((t) => ({
				id: t.stream_index,
				name: t.name,
				lang: t.language ?? undefined,
			}))}
			activeAudioTrack={activeAudioIdx}
			onAudioSelect={(track) => switchAudio(track.id)}
			chapters={fileChapters}
			knownDuration={hlsSessionId ? mediaDuration : 0}
			onSeekRestart={hlsSessionId ? seekRestart : undefined}
			{streamStats}
			{pieceMap}
			bind:transcoding
			onTranscodingChange={toggleTranscoding}
			{subtitleTracks}
			{loadingSubtitles}
			{activeTrackUrl}
			onClose={close}
			onSubtitleSelect={selectSubtitleTrack}
			onSubtitleOff={disableSubtitles}
			backdrop={playerBackdrop}
			{externalUrl}
			onReveal={() => api.streams.reveal(infoHash as string, fileIdx)}
			{startTime}
			bind:this={playerRef}
			bind:currentTime={playerTime}
			bind:paused={playerPaused}
			bind:volume={playerVolume}
			bind:duration={playerDuration}
			bind:buffered={playerBuffered}
			bind:subtitleOffset={playerSubtitleOffset}
			bind:loading={playerLoading}
			tvMode={remote.mode === "tv"}
			autoplay
		/>
	{:else}
		<VideoPlayer
			src=""
			title={playerTitle}
			topline={playerTopline}
			titleImage={item?.logo_path
				? imageUrl(item.logo_path, "original")
				: undefined}
			backdrop={playerBackdrop}
			{externalUrl}
			tvMode={remote.mode === "tv"}
			onClose={close}
		/>
	{/if}
</div>

<style>
	.player-page {
		position: fixed;
		inset: 0;
		z-index: 100;
		background: #000;
		overflow: hidden;
	}

	.error-overlay {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		color: white;
		gap: 1rem;
	}

	.error-overlay button {
		padding: 0.5rem 1.5rem;
		background: rgba(255, 255, 255, 0.1);
		border: 1px solid rgba(255, 255, 255, 0.2);
		color: white;
		border-radius: 8px;
		cursor: pointer;
	}
</style>
