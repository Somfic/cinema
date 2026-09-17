<script lang="ts">
	import { onDestroy } from "svelte";
	import { fade } from "svelte/transition";
	import type { Chapter, Stream } from "$lib/schema";
	import Hls from "hls.js";
	import { Button, Icon, toast } from "glow";
	import { OVERLAY_SELECTOR, overlays, trackOverlays } from "$lib/overlay.svelte";
	import { cast, castReachableOrigin } from "$lib/cast.svelte";
	import GradientOverlay from "./GradientOverlay.svelte";
	import Spinner from "./Spinner.svelte";
	import PlayerControls from "./PlayerControls.svelte";
	import StreamStatsPopover from "./StreamStatsPopover.svelte";

	interface SubtitleCue {
		start: number;
		end: number;
		text: string;
	}

	interface AudioTrack {
		id: number;
		name: string;
		lang?: string;
	}

	interface SubtitleTrack {
		id: string;
		language: string;
		url: string;
		score: number;
	}

	let {
		src,
		subtitles = [],
		autoplay = false,
		title,
		topline,
		titleImage,
		subtitleTracks = [],
		streams = [],
		activeStreamHash,
		audioTracks = [],
		activeAudioTrack = 0,
		chapters = [],
		onClose,
		onSubtitleSelect,
		onSubtitleOff,
		onStreamSelect,
		onAudioSelect,
		onSeekRestart,
		loadingSubtitles = false,
		activeTrackUrl,
		accent,
		backdrop,
		externalUrl,
		onReveal,
		knownDuration = 0,
		startTime = 0,
		timeOffset = 0,
		streamStats = null,
		pieceMap = [],
		transcoding = $bindable({ enabled: true, onlyAudio: false }),
		hasAudioPretranscoding = false,
		hasFullPretranscoding = false,
		onTranscodingChange,
		currentTime = $bindable(0),
		duration = $bindable(0),
		paused = $bindable(true),
		volume = $bindable(1),
		buffered = $bindable(0),
		subtitleOffset = $bindable(-0.25),
		loading = $bindable(true),
		tvMode = false,
	}: {
		src: string;
		subtitles?: SubtitleCue[];
		autoplay?: boolean;
		title?: string;
		topline?: string;
		titleImage?: string;
		subtitleTracks?: SubtitleTrack[];
		streams?: Stream[];
		activeStreamHash?: string;
		audioTracks?: AudioTrack[];
		activeAudioTrack?: number;
		chapters?: Chapter[];
		onClose?: () => void;
		onSubtitleSelect?: (track: SubtitleTrack) => void;
		onSubtitleOff?: () => void;
		onStreamSelect?: (stream: Stream) => void;
		onAudioSelect?: (track: AudioTrack) => void;
		/** Seek target fell outside the transcoded window — restart the HLS
		 *  transcode at this time instead of a native seek. */
		onSeekRestart?: (time: number) => void;
		loadingSubtitles?: boolean;
		activeTrackUrl?: string;
		accent?: string;
		backdrop?: string;
		/** Direct stream URL handed to a desktop player via the controls menu. */
		externalUrl?: string;
		/** Reveal the source file in the server's file manager. */
		onReveal?: () => void;
		knownDuration?: number;
		startTime?: number;
		/** Absolute time the current HLS session's timeline begins at. The
		 *  session itself always runs from zero, so this is what turns its
		 *  media time into a position in the file - and back again for a seek.
		 *  Zero for direct (non-transcoded) playback, which is already whole. */
		timeOffset?: number;
		currentTime?: number;
		duration?: number;
		streamStats?: {
			progress_bytes: number;
			total_bytes: number;
			download_speed_mbps: number;
			peers: number;
			finished: boolean;
		} | null;
		pieceMap?: number[];
		transcoding?: {
			enabled: boolean;
			onlyAudio: boolean;
		};
		hasAudioPretranscoding?: boolean;
		hasFullPretranscoding?: boolean;
		onTranscodingChange?: (enabled: boolean, onlyAudio: boolean) => void;
		paused?: boolean;
		volume?: number;
		buffered?: number;
		subtitleOffset?: number;
		loading?: boolean;
		/** Remote-driven TV display: hide all on-screen controls and input;
		 *  the paired phone drives playback. */
		tvMode?: boolean;
	} = $props();

	let containerEl = $state<HTMLDivElement | undefined>(undefined);
	let videoEl = $state<HTMLVideoElement | undefined>(undefined);
	let hls: Hls | null = null;

	// Casting: playback lives on a Chromecast, so the local <video> is torn
	// down and every control routes to the receiver instead. The UI stays the
	// same — it just mirrors the receiver's state.
	const casting = $derived(cast.connected);
	// Where local playback should pick up when the cast session ends.
	let localResumeAt = 0;

	// Loads the Cast SDK once per page. No-ops outside Chromium, where the
	// cast button then never appears.
	$effect(() => {
		cast.init();
	});

	// Only offer casting for real playback, and only once the SDK has seen a
	// receiver on the network.
	const castAvailable = $derived(
		!!src && cast.supported && (cast.deviceAvailable || cast.connected),
	);

	function toggleCast() {
		if (cast.connected) {
			cast.endSession();
			return;
		}
		if (!castReachableOrigin()) {
			toast.warning(
				"Open Cinema on its network address — a Chromecast can't reach localhost.",
			);
			return;
		}
		cast.requestSession();
	}

	const defaultOffset = -0.25;

	let muted = $state(false);
	let streamError = $state<string | null>(null);
	let controlsVisible = $state(true);
	let isFullscreen = $state(false);

	// Audio selection: defer to the parent callback, else fall back to the
	// native <video> audio tracks. Mirrors the previous inline menu behaviour.
	function handleAudioSelect(track: AudioTrack) {
		if (onAudioSelect) {
			onAudioSelect(track);
		} else if (videoEl) {
			const native = (videoEl as any).audioTracks;
			if (native) {
				for (let i = 0; i < native.length; i++) {
					native[i].enabled = i === track.id;
				}
			}
		}
		activeAudioTrack = track.id;
	}

	let cursorHidden = $state(false);
	let pausedIdle = $state(false);
	let pauseIdleTimeout: ReturnType<typeof setTimeout>;
	let hideTimeout: ReturnType<typeof setTimeout>;
	let volumeBeforeMute = 1;
	let clickTimeout: ReturnType<typeof setTimeout>;
	// Target of a seek that restarted the transcode. The restarted session is
	// a fresh `src`, so it loads like a first load - but `startTime` belongs to
	// the stream's original start (a resume point), and applying it here would
	// pull playback straight back to where it was before the seek.
	let seekRestartTarget: number | null = null;

	const isHls = $derived(src?.includes(".m3u8") || src?.includes("playlist"));

	const activeIndex = $derived.by(() => {
		if (!subtitles?.length) return -1;
		const t = currentTime - subtitleOffset;
		return subtitles.findIndex((s) => t >= s.start && t <= s.end);
	});

	const activeSubtitle = $derived(
		activeIndex >= 0 ? subtitles[activeIndex] : null,
	);

	// Nearby cues for crossfade (prev, current, next)
	const nearbyCues = $derived.by(() => {
		if (!subtitles?.length) return [];
		const center =
			activeIndex >= 0
				? activeIndex
				: subtitles.findIndex((s) => s.start > currentTime);
		const from = Math.max(0, center - 1);
		const to = Math.min(subtitles.length, center + 2);
		return subtitles.slice(from, to);
	});

	const progressPercent = $derived(
		duration > 0 ? (currentTime / duration) * 100 : 0,
	);
	const bufferedPercent = $derived(
		duration > 0 ? (buffered / duration) * 100 : 0,
	);
	const torrentPercent = $derived(
		streamStats && streamStats.total_bytes > 0
			? Math.round((streamStats.progress_bytes / streamStats.total_bytes) * 100)
			: 0,
	);
	let statsOpen = $state(false);

	function formatTime(seconds: number): string {
		if (!isFinite(seconds) || seconds < 0) return "0:00";
		const h = Math.floor(seconds / 3600);
		const m = Math.floor((seconds % 3600) / 60);
		const s = Math.floor(seconds % 60);
		if (h > 0)
			return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
		return `${m}:${s.toString().padStart(2, "0")}`;
	}

	export function togglePlay() {
		if (casting) {
			cast.togglePlay();
			return;
		}
		if (!videoEl) return;
		if (videoEl.paused) {
			videoEl.play().catch(() => {});
		} else {
			videoEl.pause();
		}
	}

	// Imperative controls exposed via `bind:this` so a paired remote (TV mode)
	// can drive playback through the remote store. Thin wrappers over the same
	// internals the on-screen controls use.
	export function play() {
		if (casting) {
			cast.play();
			return;
		}
		videoEl?.play().catch(() => {});
	}

	export function pause() {
		if (casting) {
			cast.pause();
			return;
		}
		videoEl?.pause();
	}

	export function seekBy(delta: number) {
		const max = duration > 0 ? duration : currentTime + delta;
		seekTo(Math.max(0, Math.min(max, currentTime + delta)));
	}

	export function setVolumeValue(value: number) {
		if (casting) {
			cast.setVolume(value);
			return;
		}
		volume = Math.max(0, Math.min(1, value));
		if (videoEl) {
			videoEl.volume = volume;
			videoEl.muted = false;
		}
		muted = volume === 0;
	}

	function withinSeekable(time: number): boolean {
		if (!videoEl) return false;
		const t = time - timeOffset;
		const r = videoEl.seekable;
		for (let i = 0; i < r.length; i++) {
			if (t >= r.start(i) - 1 && t <= r.end(i) + 0.5) return true;
		}
		return false;
	}

	export function seekTo(time: number) {
		if (casting) {
			// The receiver can only seek inside what the transcode has
			// produced so far, and that window also has a floor: a session
			// started mid-file holds nothing before its start. Outside either
			// end, restart the transcode at the target (same rule as the local
			// player's `withinSeekable` check).
			const produced = timeOffset + cast.duration;
			if (
				onSeekRestart &&
				isHls &&
				(time < timeOffset - 1 || (cast.duration > 0 && time > produced))
			) {
				seekRestartTarget = time;
				onSeekRestart(time);
				return;
			}
			cast.seekTo(time - timeOffset);
			return;
		}
		if (!videoEl) return;
		// During transcoding, a seek past the transcoded segments has no media to
		// play — restart the transcode at the target instead of a native seek.
		if (isHls && onSeekRestart && !withinSeekable(time)) {
			seekRestartTarget = time;
			onSeekRestart(time);
			return;
		}
		videoEl.currentTime = time - timeOffset;
	}

	export function toggleMute() {
		if (casting) {
			cast.toggleMute();
			return;
		}
		if (muted) {
			volume = volumeBeforeMute || 0.5;
			muted = false;
		} else {
			volumeBeforeMute = volume;
			volume = 0;
			muted = true;
		}
		if (videoEl) {
			videoEl.volume = volume;
			videoEl.muted = muted;
		}
	}

	export function toggleFullscreen() {
		if (document.fullscreenElement) {
			document.exitFullscreen();
		} else {
			// Fullscreen the player itself, not the document: the player is an
			// overlay inside the page, so fullscreening <html> would keep the
			// rail and the rest of the page chrome on screen around it.
			// Portalled popovers are moved into the player below so they stay
			// visible inside the fullscreen context.
			containerEl?.requestFullscreen();
		}
	}

	// The browser only paints the fullscreen element's subtree, and Glow's
	// popovers/menus portal to <body> — outside it. While fullscreen, adopt
	// them into the player (they are position:fixed, so they keep their
	// viewport placement) and hand them back to <body> on exit.
	function adoptOverlays() {
		if (!containerEl || document.fullscreenElement !== containerEl) return;
		for (const el of [...document.body.children]) {
			if (el.matches(OVERLAY_SELECTOR)) containerEl.appendChild(el);
		}
	}

	function releaseOverlays() {
		if (!containerEl) return;
		for (const el of [...containerEl.children]) {
			if (el.matches(OVERLAY_SELECTOR)) document.body.appendChild(el);
		}
	}

	function showControls() {
		controlsVisible = true;
		cursorHidden = false;
		clearTimeout(hideTimeout);
		if (!paused) {
			hideTimeout = setTimeout(() => {
				controlsVisible = false;
				cursorHidden = true;
			}, 3000);
		}
	}

	// A popover or menu opened from the controls portals to <body>, so pointer
	// movement over it never reaches this container and the idle timer above
	// would hide the controls, tearing down the menu the user is reading.
	$effect(() => trackOverlays());
	const overlayOpen = $derived(overlays.open);

	$effect(() => {
		if (!overlayOpen) return;
		controlsVisible = true;
		cursorHidden = false;
		clearTimeout(hideTimeout);
		return () => showControls();
	});
	function handleKeydown(e: KeyboardEvent) {
		if (!videoEl || tvMode) return;
		switch (e.key) {
			case " ":
			case "k":
				e.preventDefault();
				togglePlay();
				break;
			case "f":
				e.preventDefault();
				toggleFullscreen();
				break;
			case "Escape":
				e.preventDefault();
				if (onClose) {
					onClose();
				}
				break;
			case "j":
			case "ArrowLeft":
				e.preventDefault();
				seekTo(Math.max(0, currentTime - 10));
				showControls();
				break;
			case "l":
			case "ArrowRight":
				e.preventDefault();
				seekTo(Math.min(duration, currentTime + 10));
				showControls();
				break;
			case "ArrowUp":
				e.preventDefault();
				volume = Math.min(1, volume + 0.1);
				if (videoEl) videoEl.volume = volume;
				muted = false;
				showControls();
				break;
			case "ArrowDown":
				e.preventDefault();
				volume = Math.max(0, volume - 0.1);
				if (videoEl) videoEl.volume = volume;
				muted = volume === 0;
				showControls();
				break;
			case "m":
				e.preventDefault();
				toggleMute();
				showControls();
				break;
		}
	}

	function initVideo() {
		if (!videoEl || !src) return;
		// A cast session owns playback; don't also buffer it locally.
		if (casting) {
			if (hls) {
				hls.destroy();
				hls = null;
			}
			videoEl.pause();
			videoEl.removeAttribute("src");
			videoEl.load();
			return;
		}
		loading = true;
		streamError = null;

		if (hls) {
			hls.destroy();
			hls = null;
		}

		if (isHls && Hls.isSupported()) {
			hls = new Hls({
				debug: false,
				enableWorker: true,
				lowLatencyMode: false,
				fragLoadingMaxRetry: 5,
				fragLoadingRetryDelay: 500,
			});
			hls.loadSource(src);
			hls.attachMedia(videoEl);
			hls.on(Hls.Events.MANIFEST_PARSED, () => {
				loading = false;
				if (autoplay) videoEl?.play().catch(() => {});
			});
			hls.on(Hls.Events.ERROR, (_event: any, data: any) => {
				if (data.fatal) {
					if (data.type === Hls.ErrorTypes.NETWORK_ERROR) {
						// Network errors are often transient during torrent streaming — retry
						console.warn(
							"[hls] network error, retrying:",
							data.details,
							data.response?.code,
						);
						if (data.response?.code === 404) {
							// Could not find session, try restaring transcoding
							onTranscodingChange?.(transcoding.enabled, transcoding.onlyAudio);
						} else {
							hls?.startLoad();
						}
					} else {
						console.error(
							"[hls] fatal error:",
							data.type,
							data.details,
							data.reason,
							data.response,
						);
						loading = false;
						hls?.destroy();
						hls = null;
						streamError = `Stream failed: ${data.details ?? data.type}`;
					}
				}
			});
		} else if (isHls && videoEl.canPlayType("application/vnd.apple.mpegurl")) {
			videoEl.src = src;
		} else {
			videoEl.src = src;
			if (autoplay) videoEl.play().catch(() => {});
		}
	}

	function handleTimeUpdate() {
		if (!videoEl) return;
		// Everything this component exposes - the scrubber, subtitle timing,
		// saved progress - is a position in the file, so lift the session's
		// media time by its offset.
		currentTime = timeOffset + videoEl.currentTime;
		if (videoEl.buffered.length > 0) {
			buffered =
				timeOffset + videoEl.buffered.end(videoEl.buffered.length - 1);
		}
	}

	function handleFullscreenChange() {
		isFullscreen = !!document.fullscreenElement;
		if (isFullscreen) adoptOverlays();
		else releaseOverlays();
	}

	// Mirror the receiver's playback state onto the same fields the local
	// player writes, so the controls, scrubber, and any paired remote read one
	// consistent source regardless of where the video is actually playing.
	$effect(() => {
		if (!casting) return;
		// The receiver plays the same zero-based session the local player
		// would, so its media time needs the same lift into file positions.
		currentTime = timeOffset + cast.currentTime;
		duration =
			knownDuration > 0 ? knownDuration : timeOffset + cast.duration;
		// Everything the receiver has is playable, so the loaded window
		// doubles as the buffered bar.
		buffered = timeOffset + cast.duration;
		paused = cast.paused;
		volume = cast.volume;
		muted = cast.muted;
		loading = !cast.mediaLoaded || cast.buffering;
		localResumeAt = timeOffset + cast.currentTime;
	});

	// The probed duration can arrive after metadata has loaded (HLS transcode);
	// keep the scrubber total in sync once it does.
	$effect(() => {
		if (knownDuration > 0) duration = knownDuration;
	});

	$effect(() => {
		// `casting` is read so a session starting or ending re-runs this:
		// starting tears the local player down, ending rebuilds it.
		casting;
		if (videoEl && src) {
			initVideo();
		} else if (videoEl && !src) {
			// No source yet (e.g. switching streams) — show loading state
			loading = true;
			videoEl.removeAttribute("src");
			videoEl.load();
		}
	});

	$effect(() => {
		clearTimeout(pauseIdleTimeout);
		if (paused && !loading && duration > 0) {
			pauseIdleTimeout = setTimeout(() => {
				pausedIdle = true;
			}, 5000);
		} else {
			pausedIdle = false;
		}
	});

	$effect(() => {
		document.addEventListener("fullscreenchange", handleFullscreenChange);
		// Popovers open after the fullscreen switch portal straight to <body>;
		// catch them as they land.
		const observer = new MutationObserver(adoptOverlays);
		observer.observe(document.body, { childList: true });
		return () => {
			observer.disconnect();
			document.removeEventListener("fullscreenchange", handleFullscreenChange);
		};
	});

	onDestroy(() => {
		if (hls) {
			hls.destroy();
			hls = null;
		}
		clearTimeout(hideTimeout);
		clearTimeout(pauseIdleTimeout);
	});
</script>

{#snippet subtitleOffsetControls()}
	<div
		style="display: flex; align-items: center; justify-content: space-between; padding: 2px 4px;"
	>
		<Button
			variant="ghost"
			icon="Minus"
			onclick={() => {
				subtitleOffset -= 0.25;
			}}
		/>
		<span
			style="font-family: monospace; font-size: 0.75rem; opacity: 0.7; min-width: 3.5em; text-align: center;"
		>
			{subtitleOffset - defaultOffset > 0 ? "+" : ""}{(
				subtitleOffset - defaultOffset
			).toFixed(1)}s
		</span>
		<Button
			variant="ghost"
			icon="Plus"
			onclick={() => {
				subtitleOffset += 0.25;
			}}
		/>
	</div>
{/snippet}

{#snippet castButton()}
	{#if castAvailable}
		<Button
			variant="ghost"
			icon={cast.connected
				? { name: "Cast" as const, color: "var(--accent)" }
				: "Cast"}
			onclick={toggleCast}
		/>
	{/if}
{/snippet}

<svelte:window onkeydown={handleKeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	class="player"
	class:playing={currentTime > 0}
	class:fullscreen={isFullscreen}
	class:cursor-hidden={cursorHidden}
	style:--accent={accent ? `rgb(${accent})` : undefined}
	style:--backdrop={backdrop ? `url(${backdrop})` : "none"}
	bind:this={containerEl}
	onmousemove={showControls}
	onmouseenter={showControls}
	tabindex="-1"
>
	<!-- svelte-ignore a11y_media_has_caption -->
	<video
		bind:this={videoEl}
		style:opacity={currentTime > 0 ? 1 : 0}
		style:transition="opacity 0.5s"
		playsinline
		onclick={() => {
			if (tvMode) return;
			clearTimeout(clickTimeout);
			clickTimeout = setTimeout(togglePlay, 200);
		}}
		ondblclick={() => {
			if (tvMode) return;
			clearTimeout(clickTimeout);
			toggleFullscreen();
		}}
		ontimeupdate={handleTimeUpdate}
		onplay={() => {
			paused = false;
			showControls();
		}}
		onpause={() => {
			paused = true;
			controlsVisible = true;
		}}
		onloadedmetadata={() => {
			if (videoEl) {
				// knownDuration is set only for HLS transcode sessions, where
				// videoEl.duration covers just the segments produced so far.
				duration = knownDuration > 0 ? knownDuration : videoEl.duration;
				// Handing back from a Chromecast resumes where it left off.
				if (localResumeAt > 0) {
					videoEl.currentTime = localResumeAt - timeOffset;
					localResumeAt = 0;
					seekRestartTarget = null;
				} else if (seekRestartTarget !== null) {
					// The restarted session already begins at (or just after)
					// the target; this only trims the keyframe rounding.
					videoEl.currentTime = seekRestartTarget - timeOffset;
					seekRestartTarget = null;
				} else if (startTime > 0) {
					videoEl.currentTime = startTime - timeOffset;
				}
			}
		}}
		oncanplay={() => {
			loading = false;
			streamError = null;
		}}
		onwaiting={() => (loading = true)}
		onerror={() => {
			if (!videoEl?.error) return;
			const code = videoEl.error.code;
			// MEDIA_ERR_NETWORK (2) is transient during torrent streaming — ignore
			// MEDIA_ERR_DECODE (3) or MEDIA_ERR_SRC_NOT_SUPPORTED (4) = genuinely unplayable
			if (
				code === MediaError.MEDIA_ERR_DECODE ||
				code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED
			) {
				streamError =
					"Format not supported by browser — try enabling transcoding.";
				loading = true;
			}
		}}
	></video>

	<!-- Gradient overlay when paused but backdrop not yet showing -->
	<GradientOverlay visible={paused || loading} />

	<!-- Single centered title — shown when paused or loading -->
	<div
		class="title-overlay"
		class:visible={loading || (paused && duration > 0)}
	>
		{#if titleImage}
			<img class="title-logo" src={titleImage} alt={title || ""} />
		{:else if title}
			<span class="title-text">{title}</span>
		{/if}
	</div>

	<div class="loading-spinner" class:visible={loading}>
		<Spinner />
		{#if streamError}
			<div class="loading-progress">
				<span class="loading-detail">{streamError}</span>
			</div>
		{:else if streamStats && !streamStats.finished}
			<div class="loading-progress">
				<span class="loading-detail">
					{streamStats.download_speed_mbps.toFixed(1)} MB/s · {streamStats.peers}
					peers
				</span>
			</div>
		{/if}
	</div>

	<div class="pause-icon" class:visible={paused && !loading && currentTime > 0}>
		<Icon name="Pause" size={48} />
	</div>

	{#if casting}
		<div class="cast-overlay" transition:fade={{ duration: 150 }}>
			<Icon name="Cast" size={40} />
			<span class="cast-device">
				Playing on {cast.deviceName ?? "Chromecast"}
			</span>
			{#if cast.error}
				<span class="cast-error">{cast.error}</span>
			{/if}
		</div>
	{/if}

	<div class="subtitles-container" class:hidden={casting}>
		{#each nearbyCues as cue (cue.start)}
			<div
				class="subtitle-line"
				class:active={cue === activeSubtitle && !paused && !loading}
			>
				<p>{@html cue.text}</p>
			</div>
		{/each}
	</div>

	<!-- Top bar: title + back -->
	{#if (title || onClose) && !tvMode}
		<div class="top-bar" class:visible={controlsVisible || paused}>
			<div class="top-gradient"></div>
			<div class="top-content">
				{#if onClose}
					<Button variant="ghost" icon="ArrowLeft" onclick={onClose} />
				{/if}
				<div class="top-text">
					{#if title}
						<span class="top-title">{title}</span>
					{/if}
					{#if topline}
						<span class="top-topline">{topline}</span>
					{/if}
				</div>
				<div class="top-spacer"></div>
				<StreamStatsPopover {streamStats} />
			</div>
		</div>
	{/if}

	<!-- Bottom controls -->
	{#if !tvMode}
		<div class="controls" class:visible={controlsVisible || paused}>
			<PlayerControls
				{currentTime}
				{duration}
				{buffered}
				{paused}
				{loading}
				{volume}
				{muted}
				{streams}
				{activeStreamHash}
				{audioTracks}
				{activeAudioTrack}
				{chapters}
				{subtitleTracks}
				subtitlesActive={subtitles.length > 0}
				{activeTrackUrl}
				{transcoding}
				{hasAudioPretranscoding}
				{hasFullPretranscoding}
				{streamStats}
				{pieceMap}
				{loadingSubtitles}
				{accent}
				{isFullscreen}
				{externalUrl}
				{onReveal}
				onTogglePlay={togglePlay}
				onSeek={seekTo}
				onScrub={seekTo}
				onSetVolume={setVolumeValue}
				onToggleMute={toggleMute}
				onToggleFullscreen={toggleFullscreen}
				{onStreamSelect}
				onAudioSelect={handleAudioSelect}
				onSubtitleSelect={(t) => onSubtitleSelect?.(t as SubtitleTrack)}
				{onSubtitleOff}
				{onTranscodingChange}
				subtitleOffsetControl={subtitleOffsetControls}
				rightExtra={castButton}
			/>
		</div>

		{#if paused && !loading && duration > 0}
			<button
				class="big-play"
				onclick={togglePlay}
				aria-label="Play"
				transition:fade={{ duration: 150 }}
			></button>
		{/if}
	{/if}
</div>

<style>
	.player {
		--player-radius: 0;
		--accent: #e4e4e7;
		--accent-dim: rgba(228, 228, 231, 0.4);
		--surface: rgba(0, 0, 0, 0.75);

		position: relative;
		width: 100%;
		height: 100%;
		background: var(--backdrop) center / cover no-repeat;
		transition: background 0.8s ease;
		overflow: hidden;
		outline: none;
		user-select: none;
	}

	.player.playing {
		background: #000;
	}

	.player.fullscreen {
		border-radius: 0;
		width: 100vw;
		height: 100vh;
	}

	.player.cursor-hidden,
	.player.cursor-hidden :global(*) {
		cursor: none !important;
	}

	video {
		width: 100%;
		height: 100%;
		object-fit: contain;
		display: block;
		cursor: pointer;
	}

	/* ── Cast overlay ── */
	.cast-overlay {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.75rem;
		color: rgba(255, 255, 255, 0.85);
		pointer-events: none;
		/* Above the title treatment and the spinner (3), below the controls
		   (5) and top bar (6): while casting, what the receiver is doing is
		   the thing worth reading. */
		z-index: 4;
	}

	.cast-device {
		font-size: 1rem;
		letter-spacing: 0.01em;
	}

	.cast-error {
		font-size: 0.85rem;
		color: #fca5a5;
	}

	.subtitles-container.hidden {
		display: none;
	}

	/* ── Title overlay (single element, always mounted) ── */
	.title-overlay {
		position: absolute;
		inset: 0;
		z-index: 3;
		display: flex;
		align-items: center;
		justify-content: center;
		pointer-events: none;
		opacity: 0;
		transition: opacity 150ms ease;
	}

	.title-overlay.visible {
		opacity: 1;
	}

	.title-logo {
		max-width: 600px;
		object-fit: contain;
		filter: drop-shadow(0 0 2px rgba(0, 0, 0, 0.5));
	}

	.title-text {
		color: white;
		font-size: 2rem;
		font-weight: 700;
		text-shadow: 0 2px 12px rgba(0, 0, 0, 0.6);
	}

	/* ── Loading spinner ── */
	.loading-spinner {
		position: absolute;
		top: 75%;
		left: 50%;
		transform: translateX(-50%);
		z-index: 3;
		pointer-events: none;
		opacity: 0;
		transition: opacity 150ms ease;
		display: flex;
		flex-direction: column;
		align-items: center;
	}

	.loading-spinner.visible {
		opacity: 1;
	}

	.loading-progress {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 4px;
		margin-top: 12px;
	}

	.loading-detail {
		font-family: "JetBrains Mono", monospace;
		font-size: 0.7rem;
		color: rgba(255, 255, 255, 0.4);
	}

	/* ── Pause icon ── */
	.pause-icon {
		position: absolute;
		top: 75%;
		left: 50%;
		transform: translate(-50%, -50%);
		z-index: 3;
		pointer-events: none;
		opacity: 0;
		transition: opacity 150ms ease;
		color: rgba(255, 255, 255, 0.7);
	}

	.pause-icon.visible {
		opacity: 1;
	}

	/* ── Big play (invisible click target when paused) ── */
	.big-play {
		position: absolute;
		inset: 0;
		background: none;
		border: none;
		cursor: pointer;
		z-index: 4;
	}

	/* ── Top bar ── */
	.top-bar {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		z-index: 6;
		opacity: 0;
		transition: opacity 0.3s ease;
		pointer-events: none;
	}

	.top-bar.visible {
		opacity: 1;
		pointer-events: auto;
	}

	.top-gradient {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		height: 100px;
		background: linear-gradient(rgba(0, 0, 0, 0.7), transparent);
		pointer-events: none;
	}

	.top-content {
		position: relative;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 12px 16px;
		z-index: 1;
	}

	.top-spacer {
		flex: 1;
	}

	.top-text {
		display: flex;
		flex-direction: column;
	}

	.top-topline {
		color: rgba(255, 255, 255, 0.5);
		font-size: 0.7rem;
		line-height: 0.5rem;
		font-weight: 500;
		text-shadow: 0 1px 4px rgba(0, 0, 0, 0.6);
		letter-spacing: 0.02em;
		margin: 0;
	}

	.top-title {
		color: white;
		font-size: 1rem;
		font-weight: 600;
		text-shadow: 0 1px 4px rgba(0, 0, 0, 0.6);
		margin: 0;
	}

	/* ── Subtitles ── */
	.subtitles-container {
		position: absolute;
		bottom: 120px;
		left: 0;
		right: 0;
		z-index: 4;
		pointer-events: none;
		text-align: center;
	}

	.subtitle-line {
		position: absolute;
		bottom: 0;
		left: 50%;
		transform: translateX(-50%);
		max-width: 80%;
		white-space: pre-wrap;
		opacity: 0;
		transition: opacity 150ms ease;
	}

	.subtitle-line.active {
		opacity: 1;
	}

	.subtitle-line p {
		display: inline;
		font-family: var(--subtitle-font);
		font-size: clamp(2rem, 2.5vw, 3rem);
		font-weight: 500;
		line-height: 1.4;
		color: #ffffffdd;
		padding: 0.2em 0.5em;
		border-radius: 4px;
		text-shadow:
			0 1px 3px rgba(0, 0, 0, 1),
			0 0 12px rgba(0, 0, 0, 0.4);
		-webkit-box-decoration-break: clone;
		box-decoration-break: clone;

		filter: drop-shadow(0 0 2px rgba(0, 0, 0, 0.8));
	}

	/* ── Controls ── */
	.controls {
		position: absolute;
		bottom: 0;
		left: 0;
		right: 0;
		z-index: 5;
		opacity: 0;
		transition: opacity 0.3s ease;
		pointer-events: none;
	}

	.controls.visible {
		opacity: 1;
		pointer-events: auto;
	}
</style>
