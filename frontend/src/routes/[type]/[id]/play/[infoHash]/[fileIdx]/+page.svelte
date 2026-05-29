<script lang="ts">
	import { page } from "$app/state";
	import { goto } from "$app/navigation";
	import {
		api,
		type MediaItem,
		type MediaType,
		type SubtitleTrack,
		type SubtitleCue,
	} from "$lib/schema";
	import { getDetails, imageUrl, playStream } from "$lib/utils";
	import VideoPlayer from "$lib/components/VideoPlayer.svelte";

	interface AudioTrackInfo {
		index: number;
		stream_index: number;
		name: string;
		language: string | null;
		codec: string;
	}

	const BROWSER_SAFE_AUDIO = new Set([
		"aac",
		"mp3",
		"opus",
		"vorbis",
		"flac",
	]);

	let item = $state<MediaItem | null>(null);
	let streamUrl = $state<string | null>(null);
	let subtitleTracks = $state<SubtitleTrack[]>([]);
	let activeCues = $state<SubtitleCue[]>([]);
	let activeTrackUrl = $state<string | undefined>(undefined);
	let loadingSubtitles = $state(false);
	let error = $state<string | null>(null);
	let fileAudioTracks = $state<AudioTrackInfo[]>([]);
	let activeAudioIdx = $state(0);
	let mediaDuration = $state(0);
	let hlsSessionId = $state<string | null>(null);

	interface StreamStats {
		progress_bytes: number;
		total_bytes: number;
		download_speed_mbps: number;
		peers: number;
		finished: boolean;
	}
	let streamStats = $state<StreamStats | null>(null);
	let pieceMap = $state<number[]>([]);
	let statsUnsub: (() => void) | undefined;
	let piecesPollTimer: ReturnType<typeof setInterval> | undefined;
	let transcoding = $state({ enabled: false, onlyAudio: false });

	const backdropUrls = $derived(
		item?.backdrops?.map((b) => imageUrl(b, "original")) ?? [],
	);

	const mediaType = $derived(page.params.type as MediaType);
	const mediaId = $derived(Number(page.params.id));
	const infoHash = $derived(page.params.infoHash);
	const fileIdx = $derived(Number(page.params.fileIdx));

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
		return (
			s?.episodes?.find((e) => e.episode_number === episode)?.name ?? null
		);
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
		if (piecesPollTimer) clearInterval(piecesPollTimer);
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

		const pollPieces = async () => {
			try {
				pieceMap = await api.streams.pieces(hash, fileIdx);
			} catch {}
		};
		pollPieces();
		piecesPollTimer = setInterval(pollPieces, 2000);
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

	async function loadSubtitleTracks() {
		if (!item) return;
		loadingSubtitles = true;
		try {
			if (mediaType === "movie") {
				subtitleTracks = await api.subtitles.movie(item.id);
			} else if (season !== null && episode !== null) {
				subtitleTracks = await api.subtitles.tv(item.id, season, episode);
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
				const res = await fetch(`/api/stream/${hash}/${idx}/audio`);
				const data = await res.json();
				const tracks: AudioTrackInfo[] = data.tracks ?? [];
				if (tracks.length > 0) {
					if (tracks.length > 1) fileAudioTracks = tracks;
					if (data.duration) mediaDuration = data.duration;
					if (
						!hlsSessionId &&
						tracks[0] &&
						!BROWSER_SAFE_AUDIO.has(tracks[0].codec)
					) {
						fileAudioTracks = tracks;
						transcoding.enabled = true;
						startHlsRemux(hash, idx, 0, transcoding.onlyAudio);
					}
					clearInterval(audioPollTimer);
					audioPollTimer = undefined;
				}
			} catch {}
		};

		check();
		audioPollTimer = setInterval(check, 10_000);
	}

	let playerTime = $state(0);

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
			const result = await playStream(
				infoHash as string,
				fileIdx as number,
			);
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
		try {
			const t = startAt > 0 ? `&t=${startAt.toFixed(1)}` : "";
			const res = await fetch(
				`/api/stream/${hash}/${idx}/remux?audio=${audioIdx}${t}&onlyAudio=${onlyAudio}`,
				{ method: "POST" },
			);
			const data = await res.json();
			hlsSessionId = data.session_id;
			streamUrl = data.playlist_url;
		} catch (e: any) {
			error = e.message;
		}
	}

	function stopHlsSession() {
		if (hlsSessionId) {
			fetch(`/api/hls/${hlsSessionId}`, { method: "DELETE" }).catch(
				() => {},
			);
			hlsSessionId = null;
		}
	}

	function close() {
		if (statsUnsub) {
			statsUnsub();
			statsUnsub = undefined;
		}
		if (piecesPollTimer) {
			clearInterval(piecesPollTimer);
			piecesPollTimer = undefined;
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
			knownDuration={hlsSessionId ? mediaDuration : 0}
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
			bind:currentTime={playerTime}
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
