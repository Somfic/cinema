<script lang="ts">
	import { page } from "$app/state";
	import { replaceState } from "$app/navigation";
	import { onDestroy } from "svelte";
	import { fade } from "svelte/transition";
	import * as topbar from "$lib/topbar.svelte";
	import {
		type MediaItem,
		type MediaType,
		type Stream,
		type SubtitleTrack,
		type SubtitleCue,
		type SearchResult,
		type WatchHistoryItem,
	} from "$lib/schema";
	import { api } from "$lib/api";
	import { getDetails, imageUrl, playStream } from "$lib/utils";
	import { remote } from "$lib/remote.svelte";

	// This client is a remote-driven TV display.
	const isTv = $derived(remote.mode === "tv");
	import { Banner, Button, Spinner, Text } from "glow";
	import CyclingBackdrop from "$lib/components/CyclingBackdrop.svelte";
	import VideoPlayer from "$lib/components/VideoPlayer.svelte";
	import MediaInfo from "$lib/components/MediaInfo.svelte";
	import SeasonBrowser from "$lib/components/SeasonBrowser.svelte";
	import EpisodeDetail from "$lib/components/EpisodeDetail.svelte";

	type EmbeddedSubtitleTrack = {
		stream_index: number;
		language?: string;
		codec?: string;
	};

	// ── Core state ──
	let item = $state<MediaItem | null>(null);
	let streams = $state<Stream[]>([]);
	let selectedSeason = $state<number | null>(null);
	let selectedEpisode = $state<number | null>(null);
	let loadingStreams = $state(false);
	let error = $state<string | null>(null);
	let backdropColor = $state("9, 10, 19");
	let accentColor = $state("228, 228, 231");

	// ── Player state ──
	let selectedStream = $state<Stream | null>(null);
	let streamUrl = $state<string | null>(null);
	let subtitleTracks = $state<SubtitleTrack[]>([]);
	let activeCues = $state<SubtitleCue[]>([]);
	let activeTrackUrl = $state<string | undefined>(undefined);
	let similarItems = $state<SearchResult[]>([]);
	let resumeEntry = $state<WatchHistoryItem | null>(null);
	let playerTime = $state(0);
	let playerDuration = $state(0);
	let playerPaused = $state(true);
	let playerStartTime = $state(0);
	let loadingSubtitles = $state(false);
	let playingLocal = $state(false);

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
	interface StreamStats {
		progress_bytes: number;
		total_bytes: number;
		download_speed_mbps: number;
		peers: number;
		finished: boolean;
	}
	let streamStats = $state<StreamStats | null>(null);
	let pieceMap = $state<number[]>([]);
	// Both stats and the piece bitmap arrive via WS push.
	let statsUnsub: (() => void) | undefined;
	let piecesUnsub: (() => void) | undefined;

	let fileAudioTracks = $state<AudioTrackInfo[]>([]);
	let embeddedSubtitleTracks = $state<EmbeddedSubtitleTrack[]>([]);
	let activeAudioIdx = $state(0);
	let mediaDuration = $state(0);
	let hlsSessionId = $state<string | null>(null);
	let transcoding = $state({ enabled: false, onlyAudio: false });

	// ── Derived ──
	const slideIndex = $derived(
		selectedEpisode !== null ? 2 : selectedSeason !== null ? 1 : 0,
	);

	const backdropUrls = $derived(
		item?.backdrops
			?.filter((_, i) => i !== 1)
			.map((b) => imageUrl(b, "original")) ?? [],
	);

	const activeSeason = $derived(
		item?.seasons?.find((s) => s.season_number === selectedSeason) ?? null,
	);

	const activeEpisode = $derived(
		activeSeason?.episodes?.find(
			(e) => e.episode_number === selectedEpisode,
		) ?? null,
	);

	const playerTitle = $derived(
		item?.media_type === "tv" && activeEpisode
			? activeEpisode.name
			: item?.title,
	);

	const pageTitle = $derived(
		item?.title
			? selectedSeason !== null && selectedEpisode !== null
				? `${item.title} · S${selectedSeason} E${selectedEpisode}`
				: item.title
			: "Cinema",
	);

	const playerTopline = $derived(
		item?.media_type === "tv" &&
			selectedSeason !== null &&
			selectedEpisode !== null
			? `S${selectedSeason} E${selectedEpisode} · ${item?.title}`
			: item?.tagline || undefined,
	);

	const episodeBackdrops = $derived(
		activeEpisode?.stills?.length
			? activeEpisode.stills
					.filter((_, i) => i !== 1)
					.map((s) => imageUrl(s, "original"))
			: [],
	);

	const backdropPosition = $derived(
		selectedStream
			? "0%"
			: slideIndex === 2
				? "13%"
				: slideIndex === 1
					? "0%"
					: "-13%",
	);

	// ── Gradient color transition via @property ──
	let gradientRightEl = $state<HTMLDivElement>(undefined!);
	let gradientLeftEl = $state<HTMLDivElement>(undefined!);

	$effect(() => {
		const [r, g, b] = backdropColor.split(",").map((s) => s.trim());
		for (const el of [gradientRightEl, gradientLeftEl]) {
			if (!el) continue;
			el.style.setProperty("--tint-r", r);
			el.style.setProperty("--tint-g", g);
			el.style.setProperty("--tint-b", b);
		}
	});

	// ── Body style management ──
	$effect(() => {
		if (selectedStream) {
			document.body.style.overflow = "hidden";
			document.body.style.setProperty("display", "block", "important");
		} else {
			document.body.style.overflow = "";
			document.body.style.removeProperty("display");
		}
		return () => {
			document.body.style.overflow = "";
			document.body.style.removeProperty("display");
		};
	});

	// ── Data loading ──
	let loadedKey = "";
	$effect(() => {
		const type = page.params.type as MediaType;
		const id = Number(page.params.id);

		// Sync season/episode from the URL on every navigation (this is what lets
		// a mirrored TV follow the phone's episode selection and show the episode
		// still as its backdrop).
		selectedSeason = page.url.searchParams.has("s")
			? Number(page.url.searchParams.get("s"))
			: null;
		selectedEpisode = page.url.searchParams.has("e")
			? Number(page.url.searchParams.get("e"))
			: null;

		// Only (re)fetch when the actual title changes — a season/episode change
		// must not clear `item` and re-fetch (that would flash the loader).
		const key = `${type}/${id}`;
		if (key === loadedKey) return;
		loadedKey = key;

		item = null;
		streams = [];
		error = null;
		similarItems = [];
		getDetails(type, id)
			.then((res) => {
				item = res;
				if (selectedSeason !== null && selectedEpisode !== null) {
					// Episode was selected via URL params — navigate to episode detail
				}
				// Fetch similar + watch history in background
				api.media
					.similar(type, id)
					.then((items) => {
						similarItems = items;
					})
					.catch(() => {});
				api.watch
					.history()
					.then((items) => {
						resumeEntry =
							items.find(
								(w) =>
									w.media_type === type &&
									w.tmdb_id === id &&
									w.info_hash &&
									w.progress > 0,
							) ?? null;
					})
					.catch(() => {});
			})
			.catch((e) => (error = e.message));
	});

	// ── Navigation ──
	function selectSeason(seasonNumber: number) {
		selectedSeason = seasonNumber;
		selectedEpisode = null;
		streams = [];
		updateParams();
	}

	function selectEpisode(season: number, episode: number) {
		selectedSeason = season;
		selectedEpisode = episode;
		streams = [];
		updateParams();
	}

	function goBack(): boolean {
		if (selectedStream !== null) {
			stopPlaying();
		} else if (selectedEpisode !== null) {
			selectedEpisode = null;
			streams = [];
		} else if (selectedSeason !== null) {
			selectedSeason = null;
		} else {
			return false;
		}
		updateParams();
		return true;
	}

	topbar.setGoBack(goBack);
	onDestroy(() => {
		topbar.setGoBack(null);
	});

	function updateParams() {
		const u = new URL(window.location.href);
		if (selectedSeason !== null)
			u.searchParams.set("s", String(selectedSeason));
		else u.searchParams.delete("s");
		if (selectedEpisode !== null)
			u.searchParams.set("e", String(selectedEpisode));
		else u.searchParams.delete("e");
		replaceState(u, {});
	}

	// ── Stream loading ──
	async function loadAndPlayMovieStreams() {
		if (!item) return;
		loadingStreams = true;
		try {
			streams = await api.streams.movie(item.id);
			if (streams.length > 0) play(streams[0]);
		} catch (e: any) {
			error = e.message;
		} finally {
			loadingStreams = false;
		}
	}

	async function loadAndPlayEpisodeStreams(season: number, episode: number) {
		if (!item) return;
		loadingStreams = true;
		try {
			streams = await api.streams.tv(item.id, season, episode);
			if (streams.length > 0) play(streams[0]);
		} catch (e: any) {
			error = e.message;
		} finally {
			loadingStreams = false;
		}
	}

	async function switchStream(stream: Stream) {
		playerStartTime = playerTime;
		play(stream, true);
	}

	function playEpisode() {
		if (!item || selectedSeason == null || selectedEpisode == null) return;
		if (
			resumeEntry?.info_hash &&
			resumeEntry.season === selectedSeason &&
			resumeEntry.episode === selectedEpisode
		) {
			resume();
		} else {
			loadAndPlayEpisodeStreams(selectedSeason, selectedEpisode);
		}
	}

	async function resume() {
		if (!resumeEntry?.info_hash || !item) return;
		if (resumeEntry.season > 0) {
			selectedSeason = resumeEntry.season;
			selectedEpisode = resumeEntry.episode;
		}
		playerStartTime = resumeEntry.progress ?? 0;
		play(
			{
				info_hash: resumeEntry.info_hash,
				file_idx: resumeEntry.file_idx,
			} as Stream,
			true,
		);

		// Fetch streams in background so the stream switcher works
		try {
			if (item.media_type === "movie") {
				streams = await api.streams.movie(item.id);
			} else if (selectedSeason != null && selectedEpisode != null) {
				streams = await api.streams.tv(
					item.id,
					selectedSeason,
					selectedEpisode,
				);
			}
		} catch {}
	}

	// ── Player ──
	async function play(stream: Stream, fromResume = false) {
		if (!item) return;

		// When acting as a remote, hand playback to the paired TV instead of
		// playing here. The TV navigates to the play route and starts streaming.
		if (remote.mode === "remote" && remote.pairedId) {
			remote.cast({
				type: item.media_type,
				id: item.id,
				infoHash: stream.info_hash,
				fileIdx: stream.file_idx,
				season: item.media_type === "tv" ? selectedSeason : null,
				episode: item.media_type === "tv" ? selectedEpisode : null,
			});
			return;
		}

		if (!fromResume) playerStartTime = 0;
		selectedStream = stream;
		streamUrl = null;
		stopHlsSession();

		const u = new URL(window.location.href);
		u.searchParams.set("hash", stream.info_hash);
		u.searchParams.set("file", String(stream.file_idx));
		replaceState(u, {});

		playStream(stream.info_hash, stream.file_idx)
			.then(async (result) => {
				playingLocal = result.local;
				streamUrl = result.url;
				fileAudioTracks = [];
				activeAudioIdx = 0;
				pollAudioTracks(stream.info_hash, stream.file_idx);
				if (!result.local)
					pollStreamStats(stream.info_hash, stream.file_idx);
			})
			.catch((e) => {
				error = e.message;
				selectedStream = null;
			});

		loadSubtitles();
	}

	let audioPollTimer: ReturnType<typeof setInterval> | undefined;

	function pollAudioTracks(infoHash: string, fileIdx: number) {
		if (audioPollTimer) clearInterval(audioPollTimer);

		const check = async () => {
			try {
				const res = await fetch(
					`/api/stream/${infoHash}/${fileIdx}/audio`,
				);
				const data = await res.json();
				const tracks: AudioTrackInfo[] = data.tracks ?? [];
				const subs: EmbeddedSubtitleTrack[] = data.subtitles ?? [];
				if (tracks.length > 0) {
					if (tracks.length > 1) fileAudioTracks = tracks;
					if (data.duration) mediaDuration = data.duration;
					// Auto-switch to HLS remux if default audio codec is unsupported by the browser
					if (
						!hlsSessionId &&
						tracks[0] &&
						!BROWSER_SAFE_AUDIO.has(tracks[0].codec)
					) {
						fileAudioTracks = tracks;
						transcoding.enabled = true;
						startHlsRemux(
							infoHash,
							fileIdx,
							0,
							transcoding.onlyAudio,
						);
					}
				}
				if (subs.length > 0 && embeddedSubtitleTracks.length === 0) {
					embeddedSubtitleTracks = subs;
					// Prepend embedded tracks to subtitle list
					const embedded: SubtitleTrack[] = subs.map((s) => ({
						id: `embedded:${s.stream_index}`,
						language: s.language ?? "und",
						url: `/api/stream/${infoHash}/${fileIdx}/subtitles/${s.stream_index}`,
						score: 1000, // embedded subs are perfectly synced
					}));
					subtitleTracks = [...embedded, ...subtitleTracks];
					// Auto-select first embedded track if no track is active
					if (!activeTrackUrl && embedded.length > 0) {
						selectSubtitleTrack(embedded[0]);
					}
				}
				if (tracks.length > 0 || subs.length > 0) {
					clearInterval(audioPollTimer);
					audioPollTimer = undefined;
				}
			} catch {}
		};

		check();
		audioPollTimer = setInterval(check, 10_000);
	}

	function pollStreamStats(infoHash: string, fileIdx: number) {
		stopStreamStats();
		streamStats = null;
		pieceMap = [];

		statsUnsub = api.streamsEvents.onStats((p) => {
			if (p.info_hash !== infoHash) return;
			streamStats = {
				progress_bytes: p.progress_bytes,
				total_bytes: p.total_bytes,
				download_speed_mbps: p.download_speed_mbps,
				peers: p.peers,
				finished: p.finished,
			};
		});

		piecesUnsub = api.streamsEvents.onPieces((p) => {
			if (p.info_hash !== infoHash || p.file_idx !== fileIdx) return;
			pieceMap = p.pieces;
		});
	}

	function stopStreamStats() {
		if (statsUnsub) {
			statsUnsub();
			statsUnsub = undefined;
		}
		if (piecesUnsub) {
			piecesUnsub();
			piecesUnsub = undefined;
		}
		streamStats = null;
		pieceMap = [];
	}

	async function switchAudio(idx: number) {
		if (!selectedStream) return;
		activeAudioIdx = idx;
		await startHlsRemux(
			selectedStream.info_hash,
			selectedStream.file_idx,
			idx,
			transcoding.onlyAudio,
			playerTime,
		);
	}

	async function toggleTranscoding(enabled: boolean, onlyAudio: boolean) {
		if (!selectedStream) return;
		if (enabled) {
			await startHlsRemux(
				selectedStream.info_hash,
				selectedStream.file_idx,
				activeAudioIdx,
				onlyAudio,
				playerTime,
			);
		} else {
			stopHlsSession();
			const result = await playStream(
				selectedStream.info_hash,
				selectedStream.file_idx,
			);
			streamUrl = result.url;
		}
	}

	async function startHlsRemux(
		infoHash: string,
		fileIdx: number,
		audioIdx: number,
		onlyAudio: boolean,
		startAt = 0,
	) {
		stopHlsSession();
		streamUrl = null;
		try {
			const t = startAt > 0 ? `&t=${startAt.toFixed(1)}` : "";
			const res = await fetch(
				`/api/stream/${infoHash}/${fileIdx}/remux?audio=${audioIdx}${t}&only_audio=${onlyAudio}`,
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

	async function loadSubtitles() {
		if (!item) return;
		loadingSubtitles = true;
		try {
			if (item.media_type === "movie") {
				subtitleTracks = await api.subtitles.movie(item.id);
			} else if (selectedSeason !== null && selectedEpisode !== null) {
				subtitleTracks = await api.subtitles.tv(
					item.id,
					selectedSeason,
					selectedEpisode,
				);
			}
			if (subtitleTracks.length > 0)
				await selectSubtitleTrack(subtitleTracks[0]);
		} catch {
		} finally {
			loadingSubtitles = false;
		}
	}

	async function selectSubtitleTrack(track: SubtitleTrack) {
		loadingSubtitles = true;
		activeTrackUrl = track.url;
		try {
			if (track.id.startsWith("embedded:")) {
				// Fetch directly from embedded subtitle extraction endpoint
				const res = await fetch(track.url);
				activeCues = await res.json();
			} else {
				activeCues = await api.subtitles.cues(track.url);
			}
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

	function stopPlaying() {
		saveProgress();
		if (audioPollTimer) {
			clearInterval(audioPollTimer);
			audioPollTimer = undefined;
		}
		stopStreamStats();
		stopHlsSession();
		selectedStream = null;
		streamUrl = null;
		subtitleTracks = [];
		activeCues = [];
		activeTrackUrl = undefined;
		fileAudioTracks = [];
		embeddedSubtitleTracks = [];
		const u = new URL(window.location.href);
		u.searchParams.delete("hash");
		u.searchParams.delete("file");
		replaceState(u, {});

		// Refresh resume entry so the Continue button shows updated progress
		api.watch
			.history()
			.then((items) => {
				const type = page.params.type;
				const id = Number(page.params.id);
				resumeEntry =
					items.find(
						(w) =>
							w.media_type === type &&
							w.tmdb_id === id &&
							w.info_hash &&
							w.progress > 0,
					) ?? null;
			})
			.catch(() => {});
	}

	// ── Progress saving ──
	function saveProgress() {
		if (!item || !selectedStream || playerTime <= 0) return;
		// For TV, don't save without season/episode
		if (item.media_type === "tv" && (!selectedSeason || !selectedEpisode))
			return;
		api.watch.record({
			media_type: item.media_type,
			tmdb_id: item.id,
			title: item.title,
			poster_path: item.poster_path ?? null,
			season: selectedSeason ?? null,
			episode: selectedEpisode ?? null,
			info_hash: selectedStream.info_hash,
			file_idx: selectedStream.file_idx,
			progress: playerTime,
			duration: playerDuration,
		});
	}

	// Save when paused
	$effect(() => {
		if (playerPaused && selectedStream && playerTime > 0) {
			saveProgress();
		}
	});

	// Save periodically every 30s while playing
	$effect(() => {
		if (!selectedStream) return;
		const interval = setInterval(saveProgress, 30000);
		return () => clearInterval(interval);
	});

	// Save on page leave
	onDestroy(() => saveProgress());
</script>

<svelte:head>
	<title>{pageTitle}</title>
</svelte:head>

{#if error}
	<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
	<div onclick={() => (error = "")}>
		<Banner variant="error" label={error} />
	</div>
{/if}
{#if !item}
	<div class="loading-screen" out:fade={{ duration: 300 }}>
		<Spinner size={32} />
	</div>
{:else}
	<!-- Backdrop -->
	<div class="backdrop-container">
		<CyclingBackdrop
			images={slideIndex === 2 && episodeBackdrops.length > 0
				? episodeBackdrops
				: backdropUrls}
			overlay={slideIndex === 1 || selectedStream !== null}
			override={selectedStream ? backdropUrls[0] : undefined}
			position={isTv ? "0%" : backdropPosition}
			bind:dominantColor={backdropColor}
			bind:accentColor
		/>
	</div>
	<div
		class="gradient-right"
		class:hidden={slideIndex > 0 || selectedStream !== null || isTv}
		bind:this={gradientRightEl}
	></div>
	<div
		class="gradient-left"
		class:hidden={slideIndex !== 2 || selectedStream !== null || isTv}
		bind:this={gradientLeftEl}
	></div>

	<!-- Slider. In TV mode the screen is a remote-driven display: only the info
	     hero shows (no episode browsing, no play controls) — the phone drives. -->
	<div
		class="slider"
		class:faded={selectedStream !== null}
		style="transform: translateX({-(isTv ? 0 : slideIndex) * 100}vw)"
	>
		<!-- Page 0: Info -->
		<div class="page page-info">
			<MediaInfo
				{item}
				{loadingStreams}
				{similarItems}
				{resumeEntry}
				playing={selectedStream !== null}
				tvMode={isTv}
				onwatch={loadAndPlayMovieStreams}
				onresume={resume}
				onselectseason={selectSeason}
				onselectepisode={selectEpisode}
			/>
		</div>

		{#if !isTv}
			<!-- Page 1: Seasons + Episodes -->
			<div class="page page-episodes">
				{#if item.seasons?.length}
					<SeasonBrowser
						seasons={item.seasons}
						onscrollseason={(n) => {
							selectedSeason = n;
							updateParams();
						}}
						onselectepisode={selectEpisode}
					/>
				{/if}
			</div>

			<!-- Page 2: Episode Detail -->
			<div class="page page-episode-detail">
				{#if activeSeason && activeEpisode}
					<EpisodeDetail
						season={activeSeason}
						episode={activeEpisode}
						showTitle={item.title}
						tmdbId={item.id}
						{resumeEntry}
						{loadingStreams}
						onselectepisode={selectEpisode}
						onplay={playEpisode}
					/>
				{/if}
			</div>
		{/if}
	</div>

	<!-- Player overlay -->
	<div class="player-overlay" class:active={selectedStream !== null}>
		{#if selectedStream}
			<VideoPlayer
				src={streamUrl ?? ""}
				subtitles={activeCues}
				{streamStats}
				{pieceMap}
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
				{subtitleTracks}
				{loadingSubtitles}
				{activeTrackUrl}
				accent={accentColor}
				backdrop={activeEpisode?.stills?.[0]
					? imageUrl(activeEpisode.stills[0], "original")
					: item?.backdrops?.[0]
						? imageUrl(item.backdrops[0], "original")
						: undefined}
				startTime={playerStartTime}
				bind:transcoding
				onTranscodingChange={toggleTranscoding}
				streams={playingLocal ? [] : streams}
				activeStreamHash={selectedStream?.info_hash}
				onStreamSelect={playingLocal ? undefined : switchStream}
				bind:currentTime={playerTime}
				bind:duration={playerDuration}
				bind:paused={playerPaused}
				onClose={stopPlaying}
				onSubtitleSelect={selectSubtitleTrack}
				onSubtitleOff={disableSubtitles}
				autoplay
			/>
		{/if}
	</div>
{/if}

<style>
	.loading-screen {
		position: fixed;
		inset: 0;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	:global(body) {
		background: transparent !important;
	}

	/* ── Backdrop ── */
	.backdrop-container {
		position: fixed;
		top: 0;
		left: 0;
		width: 100%;
		height: 100%;
		z-index: 0;
	}

	@property --tint-r {
		syntax: "<number>";
		inherits: false;
		initial-value: 9;
	}
	@property --tint-g {
		syntax: "<number>";
		inherits: false;
		initial-value: 10;
	}
	@property --tint-b {
		syntax: "<number>";
		inherits: false;
		initial-value: 19;
	}

	.gradient-right,
	.gradient-left {
		position: fixed;
		inset: 0;
		z-index: 0;
		pointer-events: none;
		--tint-r: 9;
		--tint-g: 10;
		--tint-b: 19;
		transition:
			--tint-r 1.5s ease,
			--tint-g 1.5s ease,
			--tint-b 1.5s ease,
			opacity 0.5s cubic-bezier(0.4, 0, 0.2, 1);
	}

	.gradient-right {
		background: linear-gradient(
			to right,
			transparent 0%,
			transparent 35%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.6) 52%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.95) 67%,
			rgb(var(--tint-r), var(--tint-g), var(--tint-b)) 78%
		);
	}

	.gradient-left {
		background: linear-gradient(
			to left,
			transparent 0%,
			transparent 35%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.6) 52%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.95) 67%,
			rgb(var(--tint-r), var(--tint-g), var(--tint-b)) 78%
		);
	}

	.gradient-right.hidden,
	.gradient-left.hidden {
		opacity: 0;
	}

	/* ── Slider ── */
	.slider {
		position: relative;
		z-index: 1;
		display: flex;
		width: 300vw;
		height: 100%;
		overflow: hidden;
		transition:
			transform 0.5s cubic-bezier(0.4, 0, 0.2, 1),
			opacity 0.5s ease;
	}

	.slider.faded {
		opacity: 0;
		pointer-events: none;
	}

	.page {
		width: 100vw;
		height: 100%;
		flex-shrink: 0;
		overflow-y: auto;
	}

	.page-info {
		display: flex;
		justify-content: flex-end;
		position: relative;
	}

	.page-episodes {
		display: flex;
	}

	.page-episode-detail {
		display: flex;
		overflow: hidden;
	}

	/* ── Player ── */
	.player-overlay {
		position: fixed;
		top: 0;
		left: 0;
		width: 100vw;
		height: 100vh;
		z-index: 10;
		opacity: 0;
		pointer-events: none;
		transition: opacity 0.5s ease;
		overflow: hidden;
	}

	.player-overlay.active {
		opacity: 1;
		pointer-events: auto;
	}

	@media (max-width: 768px) {
		.gradient-right,
		.gradient-left {
			background: linear-gradient(
				to bottom,
				transparent 0%,
				transparent 30%,
				rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.7) 50%,
				rgb(var(--tint-r), var(--tint-g), var(--tint-b)) 70%
			);
		}

		.page-info {
			justify-content: flex-start;
		}

		.page-episodes {
			flex-direction: column;
		}

		.page-episode-detail {
			flex-direction: column;
		}
	}
</style>
