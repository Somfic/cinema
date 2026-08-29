<script lang="ts">
	import { page } from "$app/state";
	import { replaceState } from "$app/navigation";
	import { onDestroy } from "svelte";
	import { fade } from "svelte/transition";
	import * as topbar from "$lib/topbar.svelte";
	import {
		type MediaItem,
		type Stream,
		type WatchHistoryItem,
		type MediaType,
		type TranscodingOption,
	} from "$lib/schema";
	import { api } from "$lib/api";
	import { getDetails, imageUrl, rgbToHex } from "$lib/utils";
	import { remote } from "$lib/remote.svelte";
	import { settings } from "$lib/settings.svelte";
	import { PlaybackSession } from "$lib/playback.svelte";

	import { Banner, Spinner, Glow } from "glow";
	import CyclingBackdrop from "$lib/components/CyclingBackdrop.svelte";
	import VideoPlayer from "$lib/components/VideoPlayer.svelte";
	import MediaInfo from "$lib/components/MediaInfo.svelte";
	import SeasonBrowser from "$lib/components/SeasonBrowser.svelte";
	import EpisodeDetail from "$lib/components/EpisodeDetail.svelte";
	import DownloadModal from "$lib/components/DownloadModal.svelte";

	// This client is a remote-driven TV display.
	const isTv = $derived(remote.mode === "tv");

	// ── Download modal ──
	let downloadModalState = $state<
		true | { season: number; episode: number } | null
	>(null);

	// ── Core state ──
	let item = $state<MediaItem | null>(null);
	let streams = $state<Stream[]>([]);
	let selectedSeason = $state<number | null>(null);
	let selectedEpisode = $state<number | null>(null);
	let loadingStreams = $state(false);
	let error = $state<string | null>(null);
	let backdropColor = $state("9, 10, 19");
	let accentColor = $state("228, 228, 231");
	let palette = $state<string[]>([]);

	// ── Player state ──
	let selectedStream = $state<Stream | null>(null);
	let resumeEntry = $state<WatchHistoryItem | null>(null);
	let playerTime = $state(0);
	let playerDuration = $state(0);
	let playerPaused = $state(true);
	let playerStartTime = $state(0);

	const session = new PlaybackSession({
		item: () => item,
		season: () => selectedSeason,
		episode: () => selectedEpisode,
		currentStream: () => selectedStream,
		onError: (msg) => (error = msg),
	});

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
		activeSeason?.episodes?.find((e) => e.episode_number === selectedEpisode) ??
			null,
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

	// Reveal the loading glow only once loading has lasted >500ms — most titles
	// load near-instantly, so flashing the glow every time is jarring. Until then
	// the glow stays black (indistinguishable from the dark background); when it
	// flips true the Glow's own `transition` morphs black → the loading palette
	// for us, and if the title loads first it morphs straight to its colors.
	let loadingSlow = $state(false);
	$effect(() => {
		if (item) {
			loadingSlow = false;
			return;
		}
		loadingSlow = false;
		const t = setTimeout(() => (loadingSlow = true), 500);
		return () => clearTimeout(t);
	});

	// ── Glow backdrop palette ──
	// A dark→vibrant ramp built from the extracted backdrop colors: the darkened
	// dominant as the base/gap, the vibrant accent (dimmed → full → lightened) as
	// the flowing light. Kept dim so text over it stays readable.
	//
	// While loading there are no extracted colors yet, so use black (invisible)
	// for the first 500ms, then a vivid cinema-purple palette for a slow load.
	const BLACK = "#000000";
	const BLACK_COLORS = [BLACK, BLACK, BLACK, BLACK, BLACK];
	const DEFAULT_GLOW_BG = "#0a0616";
	const DEFAULT_GLOW_COLORS = [
		"#1a0033",
		"#5b2a9d",
		"#8b6ded",
		"#5e7bff",
		"#c4b5fd",
	];
	const glowBg = $derived(
		item ? rgbToHex(backdropColor, 1.4) : loadingSlow ? DEFAULT_GLOW_BG : BLACK,
	);
	// Lift a "r, g, b" swatch so its brightest channel reaches `targetMax`, keeping
	// hue. Only ever brightens (never dims), so a dark backdrop (e.g. a deep-blue
	// poster) still yields a visibly glowing hot stop instead of a near-black ramp.
	function litHex(rgb: string, targetMax: number): string {
		const [r, g, b] = rgb.split(",").map((s) => Number(s.trim()));
		const mx = Math.max(r, g, b, 1);
		return rgbToHex(rgb, Math.max(1, targetMax / mx));
	}
	const glowColors = $derived.by(() => {
		if (!item) return loadingSlow ? DEFAULT_GLOW_COLORS : BLACK_COLORS;
		// Fall back to the old single-accent brightness ramp when no palette was
		// extracted.
		if (!palette.length) {
			return [
				rgbToHex(backdropColor, 1.4),
				rgbToHex(accentColor, 0.8),
				rgbToHex(accentColor, 1.2),
				rgbToHex(accentColor, 1.7),
			];
		}
		// Glow takes 5 stops. Reserve the first for the dark dominant base, then
		// sample up to 4 swatches evenly from the dark→light palette (keeps both
		// the darkest and the lightest).
		const MAX = 4;
		const swatches =
			palette.length <= MAX
				? palette
				: Array.from(
						{ length: MAX },
						(_, k) =>
							palette[Math.round((k * (palette.length - 1)) / (MAX - 1))],
					);
		// Lift each swatch toward a rising brightness target (150 → 240 on its
		// brightest channel) so the ramp always reaches a visible hot stop.
		const n = swatches.length;
		const lit = swatches.map((c, i) =>
			litHex(c, 150 + (n === 1 ? 1 : i / (n - 1)) * 90),
		);
		return [rgbToHex(backdropColor, 1.4), ...lit];
	});

	// Perceptual brightness (luma, 0–1) of the accent color.
	const accentLuma = $derived.by(() => {
		const [r, g, b] = accentColor.split(",").map((s) => Number(s.trim()));
		return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
	});
	// A fresh backdrop pattern each time a title page opens.
	//
	// Listed explicitly rather than derived from the library's PATTERN_NAMES:
	// this is an editorial choice, not a mirror of whatever glow ships. `dither`
	// and `halftone` are deliberately absent — they are dense, high-contrast
	// fields and this page puts a title, metadata and buttons over the glow. A
	// pattern added to glow later should not silently start appearing here.
	//
	// Picked at component init rather than in onMount: the pattern only changes
	// what the WebGL canvas draws, never the server-rendered markup, so server
	// and client disagreeing on it cannot cause a hydration mismatch.
	const GLOW_PATTERNS = [
		"fold",
		"aurora",
		"curl",
		"ink",
		"oilfilm",
		"marble",
		"caustics",
		"prism",
		"soapfilm",
		"mesh",
	] as const;
	const glowPattern =
		GLOW_PATTERNS[Math.floor(Math.random() * GLOW_PATTERNS.length)];

	// Lerp between "ray" mode (morph 0, the pattern's resting form) for dark
	// accents and its morphed form for bright accents. smoothstep over the
	// mid-brightness band so the transition is gradual, not a hard switch. During
	// loading, use the resting form for a clean ambient look.
	//
	// This drives `morph` rather than `ribbon`: ribbon only exists on the `fold`
	// pattern, while morph is the same axis generalised across all of them, so
	// the brightness response works whichever pattern was drawn.
	const glowMorph = $derived.by(() => {
		if (!item) return 0;
		const t = Math.max(0, Math.min(1, (accentLuma - 0.35) / 0.4));
		return t * t * (3 - 2 * t);
	});

	// Which side the backdrop fades into — mirrors the .gradient-right/.left
	// visibility below. `full` fills the screen with glow: while the title loads
	// (behind the spinner) and in season-select mode (slide 1, over the blurred
	// backdrop). `none` when nothing is shown (playing / TV).
	const glowSide = $derived(
		!item
			? "full"
			: selectedStream !== null || isTv
				? "none"
				: slideIndex === 0
					? "right"
					: slideIndex === 2
						? "left"
						: "full",
	);
	const glowVisible = $derived(glowSide !== "none");

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
		getDetails(type, id)
			.then((res) => {
				item = res;
				if (selectedSeason !== null && selectedEpisode !== null) {
					// Episode was selected via URL params — navigate to episode detail
				}
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
			streams = await api.streams.movie(item.tmdb_id);
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
			streams = await api.streams.tv(item.tmdb_id, season, episode);
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
			{ startAt: playerStartTime, transcoding: resumeEntry.transcoding },
		);

		// Fetch streams in background so the stream switcher works
		try {
			if (item.media_type === "movie") {
				streams = await api.streams.movie(item.tmdb_id);
			} else if (selectedSeason != null && selectedEpisode != null) {
				streams = await api.streams.tv(
					item.tmdb_id,
					selectedSeason,
					selectedEpisode,
				);
			}
		} catch {}
	}

	// ── Player ──
	function play(
		stream: Stream,
		fromResume = false,
		startOptions?: { startAt?: number; transcoding?: TranscodingOption },
	) {
		if (!item) return;

		// When acting as a remote, hand playback to the paired TV instead of
		// playing here. The TV navigates to the play route and starts streaming.
		if (remote.mode === "remote" && remote.pairedId) {
			remote.cast({
				type: item.media_type,
				id: item.tmdb_id,
				infoHash: stream.info_hash,
				fileIdx: stream.file_idx,
				season: item.media_type === "tv" ? selectedSeason : null,
				episode: item.media_type === "tv" ? selectedEpisode : null,
			});
			return;
		}

		if (!fromResume) playerStartTime = 0;
		selectedStream = stream;

		const u = new URL(window.location.href);
		u.searchParams.set("hash", stream.info_hash);
		u.searchParams.set("file", String(stream.file_idx));
		replaceState(u, {});

		session
			.start(stream, startOptions)
			.then(() => session.loadSubtitles())
			.catch((e: Error) => {
				error = e.message;
				selectedStream = null;
			});
	}

	function stopPlaying() {
		session.saveProgress(playerTime, playerDuration);
		session.stop();
		selectedStream = null;
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
	// Save when paused
	$effect(() => {
		if (playerPaused && selectedStream && playerTime > 0) {
			session.saveProgress(playerTime, playerDuration);
		}
	});

	// Save periodically every 30s while playing
	$effect(() => {
		if (!selectedStream) return;
		const interval = setInterval(
			() => session.saveProgress(playerTime, playerDuration),
			30000,
		);
		return () => clearInterval(interval);
	});

	// Save on page leave and tear down the session (audio poll timer, stats
	// subscriptions, backend HLS session) so navigating away doesn't leak them.
	onDestroy(() => {
		session.saveProgress(playerTime, playerDuration);
		session.stop();
	});
</script>

<svelte:head>
	<title>{pageTitle}</title>
</svelte:head>

{#if error}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<div role="button" tabindex="0" onclick={() => (error = "")}>
		<Banner variant="error" label={error} />
	</div>
{/if}
<!-- Single Glow instance, kept mounted across loading → loaded so the WebGL
     context is never torn down and recreated. `full` fills the screen behind the
     loading spinner; once loaded it fades into the backdrop's content side. -->
{#if settings.animations.glow}
	<div
		class="glow-fade"
		class:hidden={!glowVisible}
		class:full={glowSide === "full"}
		class:left={glowSide === "left"}
		class:right={glowSide === "right"}
	>
		<Glow
			pattern={glowPattern}
			colors={glowColors}
			bgColor={glowBg}
			rotation={52}
			zoom={7}
			morph={glowMorph}
			ribbonWidth={1.3}
			transition={5000}
			speed={glowVisible ? 1 : 0}
		/>
	</div>
{/if}
{#if !item}
	<div class="loading-screen" out:fade={{ duration: 300 }}>
		<Spinner size={32} />
	</div>
{:else}
	<!-- Backdrop -->
	<div class="backdrop-container" class:blurred={slideIndex === 1}>
		<CyclingBackdrop
			images={slideIndex === 2 && episodeBackdrops.length > 0
				? episodeBackdrops
				: backdropUrls}
			overlay={slideIndex === 1 || selectedStream !== null}
			override={selectedStream ? backdropUrls[0] : undefined}
			position={isTv ? "0%" : backdropPosition}
			bind:dominantColor={backdropColor}
			bind:accentColor
			bind:palette
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
				{resumeEntry}
				playing={selectedStream !== null}
				tvMode={isTv}
				onwatch={loadAndPlayMovieStreams}
				onresume={resume}
				onselectseason={selectSeason}
				onselectepisode={selectEpisode}
				ondownload={() => {
					downloadModalState = true;
				}}
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
						tmdbId={item.tmdb_id}
						{resumeEntry}
						{loadingStreams}
						onselectepisode={selectEpisode}
						onplay={playEpisode}
						ondownload={(season, episode) => {
							downloadModalState = {
								season,
								episode,
							};
						}}
					/>
				{/if}
			</div>
		{/if}
	</div>

	<!-- Player overlay -->
	<div class="player-overlay" class:active={selectedStream !== null}>
		{#if selectedStream}
			<VideoPlayer
				src={session.streamUrl ?? ""}
				subtitles={session.activeCues}
				streamStats={session.streamStats}
				pieceMap={session.pieceMap}
				title={playerTitle}
				topline={playerTopline}
				titleImage={item?.logo_path
					? imageUrl(item.logo_path, "original")
					: undefined}
				audioTracks={session.fileAudioTracks.map((t) => ({
					id: t.stream_index,
					name: t.name,
					lang: t.language ?? undefined,
				}))}
				activeAudioTrack={session.activeAudioIdx}
				onAudioSelect={(track) => session.switchAudio(track.id, playerTime)}
				chapters={session.fileChapters}
				knownDuration={session.hlsSessionId ? session.mediaDuration : 0}
				onSeekRestart={session.hlsSessionId
					? (t) => session.seekRestart(t)
					: undefined}
				subtitleTracks={session.subtitleTracks}
				loadingSubtitles={session.loadingSubtitles}
				activeTrackUrl={session.activeTrackUrl}
				accent={accentColor}
				backdrop={activeEpisode?.stills?.[0]
					? imageUrl(activeEpisode.stills[0], "original")
					: item?.backdrops?.[0]
						? imageUrl(item.backdrops[0], "original")
						: undefined}
				startTime={playerStartTime}
				bind:transcoding={session.transcoding}
				hasAudioPretranscoding={session.hasAudioPretranscoding}
				hasFullPretranscoding={session.hasFullPretranscoding}
				onTranscodingChange={(enabled, onlyAudio) =>
					session.toggleTranscoding(enabled, onlyAudio, playerTime)}
				streams={session.playingLocal ? [] : streams}
				activeStreamHash={selectedStream?.info_hash}
				externalUrl={api.urls.stream(
					selectedStream.info_hash,
					selectedStream.file_idx,
				)}
				onReveal={() =>
					selectedStream &&
					api.streams.reveal(selectedStream.info_hash, selectedStream.file_idx)}
				onStreamSelect={session.playingLocal ? undefined : switchStream}
				bind:currentTime={playerTime}
				bind:duration={playerDuration}
				bind:paused={playerPaused}
				onClose={stopPlaying}
				onSubtitleSelect={(t) => session.selectSubtitleTrack(t)}
				onSubtitleOff={() => session.disableSubtitles()}
				autoplay
			/>
		{/if}
	</div>
{/if}

{#if item}
	<DownloadModal
		bind:open={
			() => downloadModalState !== null,
			(open) => {
				if (!open) {
					downloadModalState = null;
				}
			}
		}
		tmdbId={item.tmdb_id}
		mediaType={item.media_type}
		season={typeof downloadModalState === "object"
			? downloadModalState?.season
			: null}
		episode={typeof downloadModalState === "object"
			? downloadModalState?.episode
			: null}
	/>
{/if}

<style>
	.loading-screen {
		position: fixed;
		inset: 0;
		z-index: 4;
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
		transition: filter 0.5s ease;
	}

	/* Season-select mode: the glow is the backdrop, with the image blurred
	   softly behind it. `scale` hides the transparent edge bleed the blur
	   would otherwise pull in. */
	.backdrop-container.blurred {
		filter: blur(24px);
		transform: scale(1.08);
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
		z-index: 2;
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
			transparent 50%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.38) 64%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.54) 77%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.6) 86%
		);
	}

	.gradient-left {
		background: linear-gradient(
			to left,
			transparent 0%,
			transparent 50%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.38) 64%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.54) 77%,
			rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.6) 86%
		);
	}

	.gradient-right.hidden,
	.gradient-left.hidden {
		opacity: 0;
	}

	/* Animated Glow the backdrop fades into, revealed on the content side by a
	   mask matching the scrim's alpha ramp. Sits behind the scrim gradients. */
	.glow-fade {
		position: fixed;
		inset: 0;
		/* Above the backdrop image (z0), below the scrim gradients (z2) and
		   content (z3). It's earlier in the DOM than .backdrop-container now
		   (single instance kept across load), so it needs an explicit z-index. */
		z-index: 1;
		pointer-events: none;
		opacity: 1;
		transition: opacity 0.5s cubic-bezier(0.4, 0, 0.2, 1);
	}

	.glow-fade.hidden {
		opacity: 0;
	}

	/* Loading state: no side mask — the glow fills the screen behind the spinner,
	   dimmed so it reads as an ambient background. */
	.glow-fade.full {
		mask-image: none;
		-webkit-mask-image: none;
		opacity: 0.6;
	}

	.glow-fade.right {
		mask-image: linear-gradient(
			to right,
			rgba(0, 0, 0, 0.13) 0%,
			rgba(0, 0, 0, 0.13) 50%,
			rgba(0, 0, 0, 0.6) 64%,
			rgba(0, 0, 0, 0.95) 77%,
			#000 86%
		);
		-webkit-mask-image: linear-gradient(
			to right,
			rgba(0, 0, 0, 0.13) 0%,
			rgba(0, 0, 0, 0.13) 50%,
			rgba(0, 0, 0, 0.6) 64%,
			rgba(0, 0, 0, 0.95) 77%,
			#000 86%
		);
	}

	.glow-fade.left {
		mask-image: linear-gradient(
			to left,
			rgba(0, 0, 0, 0.13) 0%,
			rgba(0, 0, 0, 0.13) 50%,
			rgba(0, 0, 0, 0.6) 64%,
			rgba(0, 0, 0, 0.95) 77%,
			#000 86%
		);
		-webkit-mask-image: linear-gradient(
			to left,
			rgba(0, 0, 0, 0.13) 0%,
			rgba(0, 0, 0, 0.13) 50%,
			rgba(0, 0, 0, 0.6) 64%,
			rgba(0, 0, 0, 0.95) 77%,
			#000 86%
		);
	}

	/* ── Slider ── */
	.slider {
		position: relative;
		z-index: 3;
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
				transparent 45%,
				rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.42) 62%,
				rgba(var(--tint-r), var(--tint-g), var(--tint-b), 0.6) 80%
			);
		}

		.glow-fade.left,
		.glow-fade.right {
			mask-image: linear-gradient(
				to bottom,
				rgba(0, 0, 0, 0.13) 0%,
				rgba(0, 0, 0, 0.13) 45%,
				rgba(0, 0, 0, 0.7) 62%,
				#000 80%
			);
			-webkit-mask-image: linear-gradient(
				to bottom,
				rgba(0, 0, 0, 0.13) 0%,
				rgba(0, 0, 0, 0.13) 45%,
				rgba(0, 0, 0, 0.7) 62%,
				#000 80%
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
