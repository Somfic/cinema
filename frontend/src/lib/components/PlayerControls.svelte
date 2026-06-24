<script lang="ts">
	import type { Snippet } from "svelte";
	import type { Chapter } from "$lib/schema";
	import { Button, PopoverMenu, type PopoverMenuEntry } from "glow";

	interface AudioTrack {
		id: number;
		name: string;
		lang?: string;
	}
	interface SubtitleTrack {
		id: string;
		language: string;
		url: string;
	}
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	type StreamOption = any;

	let {
		currentTime,
		duration,
		buffered = 0,
		paused,
		loading = false,
		volume,
		muted,
		streams = [],
		activeStreamHash,
		audioTracks = [],
		activeAudioTrack = 0,
		subtitleTracks = [],
		subtitlesActive = false,
		chapters = [],
		activeTrackUrl,
		transcoding = { enabled: false, onlyAudio: false },
		streamStats = null,
		pieceMap = [],
		loadingSubtitles = false,
		accent,
		volumeAlwaysOpen = false,
		isFullscreen = false,
		externalUrl,
		onReveal,
		onTogglePlay,
		onSeek,
		onScrub,
		onSetVolume,
		onToggleMute,
		onToggleFullscreen,
		onStreamSelect,
		onAudioSelect,
		onSubtitleSelect,
		onSubtitleOff,
		onTranscodingChange,
		subtitleOffsetControl,
		rightExtra,
	}: {
		currentTime: number;
		duration: number;
		buffered?: number;
		paused: boolean;
		loading?: boolean;
		volume: number;
		muted: boolean;
		streams?: StreamOption[];
		activeStreamHash?: string;
		audioTracks?: AudioTrack[];
		activeAudioTrack?: number;
		subtitleTracks?: SubtitleTrack[];
		subtitlesActive?: boolean;
		chapters?: Chapter[];
		activeTrackUrl?: string;
		transcoding?: { enabled: boolean; onlyAudio: boolean };
		streamStats?: { total_bytes: number; finished: boolean } | null;
		pieceMap?: number[];
		loadingSubtitles?: boolean;
		accent?: string;
		volumeAlwaysOpen?: boolean;
		isFullscreen?: boolean;
		/** Direct stream URL (path or absolute) to hand off to a desktop player.
		 *  When set, an "open in external player" button is shown. */
		externalUrl?: string;
		/** Reveal the source file in the server's file manager. When set, a
		 *  "Reveal in file explorer" entry is added to the external menu. */
		onReveal?: () => void;
		onTogglePlay: () => void;
		onSeek: (time: number) => void;
		onScrub?: (time: number) => void;
		onSetVolume: (value: number) => void;
		onToggleMute: () => void;
		onToggleFullscreen?: () => void;
		onStreamSelect?: (stream: StreamOption) => void;
		onAudioSelect?: (track: AudioTrack) => void;
		onSubtitleSelect?: (track: SubtitleTrack) => void;
		onSubtitleOff?: () => void;
		onTranscodingChange?: (enabled: boolean, onlyAudio: boolean) => void;
		subtitleOffsetControl?: Snippet;
		rightExtra?: Snippet;
	} = $props();

	let trackEl = $state<HTMLDivElement | undefined>(undefined);
	let scrubbing = $state(false);
	let scrubValue = $state(0);

	const displayTime = $derived(scrubbing ? scrubValue : currentTime);
	const progressPercent = $derived(
		duration > 0 ? (displayTime / duration) * 100 : 0,
	);
	const bufferedPercent = $derived(
		duration > 0 ? (buffered / duration) * 100 : 0,
	);

	// Only show chapters when there's more than one and we know the duration.
	const showChapters = $derived(chapters.length > 1 && duration > 0);

	// How far (0–1) a given time has progressed within a single chapter — used to
	// fill each YouTube-style segment independently.
	function segFraction(chapter: Chapter, time: number): number {
		const span = chapter.end - chapter.start;
		if (span <= 0) return 0;
		return Math.max(0, Math.min(1, (time - chapter.start) / span));
	}

	function chapterAt(time: number): Chapter | undefined {
		if (!showChapters) return undefined;
		return (
			chapters.find((c) => time >= c.start && time < c.end) ??
			(time >= chapters[chapters.length - 1].start
				? chapters[chapters.length - 1]
				: undefined)
		);
	}

	const currentChapter = $derived(chapterAt(displayTime));

	// Hover state for the chapter tooltip while pointing along the track.
	let hovering = $state(false);
	let hoverTime = $state(0);
	const hoverPercent = $derived(
		duration > 0 ? (hoverTime / duration) * 100 : 0,
	);
	const hoverChapter = $derived(chapterAt(hoverTime));

	function formatTime(seconds: number): string {
		if (!isFinite(seconds) || seconds < 0) return "0:00";
		const h = Math.floor(seconds / 3600);
		const m = Math.floor((seconds % 3600) / 60);
		const s = Math.floor(seconds % 60);
		if (h > 0)
			return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
		return `${m}:${s.toString().padStart(2, "0")}`;
	}

	function posFromEvent(clientX: number): number | null {
		if (!trackEl || !duration) return null;
		const rect = trackEl.getBoundingClientRect();
		const pct = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
		return pct * duration;
	}
	function onPointerDown(e: PointerEvent & { currentTarget: HTMLDivElement }) {
		const t = posFromEvent(e.clientX);
		if (t == null) return;
		scrubbing = true;
		scrubValue = t;
		onScrub?.(t);
		e.currentTarget.setPointerCapture(e.pointerId);
	}
	function onPointerMove(e: PointerEvent) {
		const t = posFromEvent(e.clientX);
		if (t == null) return;
		hovering = true;
		hoverTime = t;
		if (!scrubbing) return;
		scrubValue = t;
		onScrub?.(t);
	}
	function onPointerLeave() {
		if (!scrubbing) hovering = false;
	}
	function onPointerUp(e: PointerEvent) {
		if (!scrubbing) return;
		const t = posFromEvent(e.clientX) ?? scrubValue;
		scrubbing = false;
		onSeek(t);
	}

	const activeResolution = $derived(
		streams.find((s: StreamOption) => s.info_hash === activeStreamHash)
			?.resolution ?? null,
	);

	const resolutions = $derived.by(() => {
		const seen = new Set<string>();
		const result: string[] = [];
		for (const s of streams) {
			const res = s.resolution;
			if (res && !seen.has(res)) {
				seen.add(res);
				result.push(res);
			}
		}
		const order: Record<string, number> = {
			"4K": 4,
			"2160p": 4,
			"1080p": 3,
			"720p": 2,
			"480p": 1,
		};
		return result.sort((a, b) => (order[b] ?? 0) - (order[a] ?? 0));
	});

	const TRANSCODE_OPTIONS = [
		{ value: "none", label: "None", icon: "Ban" as const },
		{ value: "audio", label: "Audio", icon: "AudioLines" as const },
		{ value: "both", label: "Audio + video", icon: "Film" as const },
	];

	const transcodingMode = $derived(
		!transcoding.enabled ? "none" : transcoding.onlyAudio ? "audio" : "both",
	);

	function setTranscodingMode(mode: string) {
		const enabled = mode !== "none";
		const onlyAudio = mode === "audio";
		transcoding.enabled = enabled;
		transcoding.onlyAudio = onlyAudio;
		onTranscodingChange?.(enabled, onlyAudio);
	}

	const streamMenuItems = $derived<PopoverMenuEntry[]>([
		...(resolutions.length > 1
			? [
					{ kind: "header" as const, label: "Quality" },
					...resolutions.map((res) => ({
						kind: "item" as const,
						label: res,
						selected: res === activeResolution,
						onclick: () => {
							const best = streams.find(
								(s: StreamOption) => s.resolution === res,
							);
							if (best) onStreamSelect?.(best);
						},
					})),
				]
			: []),
		{ kind: "header" as const, label: "Sources" },
		...(activeResolution
			? streams.filter((s: StreamOption) => s.resolution === activeResolution)
			: streams
		)
			.slice(0, 8)
			.map((stream: StreamOption) => ({
				kind: "item" as const,
				label: `${stream.source}`,
				description: [stream.codec, stream.audio, stream.source_type]
					.filter(Boolean)
					.join(" · "),
				shortcut: stream.size_display ?? undefined,
				selected: stream.info_hash === activeStreamHash,
				onclick: () => onStreamSelect?.(stream),
			})),
		...(onTranscodingChange
			? [
					{ kind: "header" as const, label: "Transcoding" },
					{
						kind: "radio" as const,
						options: TRANSCODE_OPTIONS,
						value: transcodingMode,
						iconOnly: true,
						onChange: setTranscodingMode,
					},
				]
			: []),
	]);

	const audioSubtitleMenuItems = $derived<PopoverMenuEntry[]>([
		...(audioTracks.length > 1
			? [
					{ kind: "header" as const, label: "Audio" },
					...audioTracks.map((track) => ({
						kind: "item" as const,
						label: track.name,
						description: track.lang ?? undefined,
						selected: track.id === activeAudioTrack,
						onclick: () => onAudioSelect?.(track),
					})),
					"divider" as const,
				]
			: []),
		...(subtitleTracks.length > 0
			? [
					{ kind: "header" as const, label: "Subtitles" },
					{
						kind: "item" as const,
						label: "Off",
						selected: !subtitlesActive,
						onclick: () => onSubtitleOff?.(),
					},
					...subtitleTracks.map((track) => {
						const isEmbedded = track.id.startsWith("embedded:");
						const dupes = subtitleTracks.filter(
							(t) =>
								t.language === track.language &&
								t.id.startsWith("embedded:") === isEmbedded,
						);
						const suffix =
							dupes.length > 1 ? ` #${dupes.indexOf(track) + 1}` : "";
						return {
							kind: "item" as const,
							label: `${track.language}${suffix}`,
							description: isEmbedded ? "Embedded" : undefined,
							selected: track.url === activeTrackUrl,
							onclick: () => onSubtitleSelect?.(track),
						};
					}),
					...(subtitlesActive && subtitleOffsetControl
						? [
								"divider" as const,
								{
									kind: "custom" as const,
									render: subtitleOffsetControl,
								},
							]
						: []),
				]
			: []),
	]);

	// Resolve to an absolute URL a desktop player on the same network can reach.
	function absoluteExternalUrl(): string {
		if (!externalUrl) return "";
		return /^https?:\/\//.test(externalUrl)
			? externalUrl
			: `${window.location.origin}${externalUrl}`;
	}

	let copied = $state(false);
	let copiedTimer: ReturnType<typeof setTimeout>;

	async function copyExternalUrl() {
		try {
			await navigator.clipboard.writeText(absoluteExternalUrl());
			copied = true;
			clearTimeout(copiedTimer);
			copiedTimer = setTimeout(() => (copied = false), 1500);
		} catch {
			// Clipboard may be unavailable (insecure context) — ignore.
		}
	}

	// Download an .m3u playlist pointing at the stream. Desktop VLC/mpv register
	// as the .m3u handler, so opening the downloaded file launches the user's
	// default player — the reliable cross-platform alternative to a `vlc://`
	// scheme link, which only mobile VLC honours.
	function downloadM3u() {
		const url = absoluteExternalUrl();
		if (!url) return;
		const content = `#EXTM3U\n#EXTINF:-1,Cinema stream\n${url}\n`;
		const blob = new Blob([content], { type: "audio/x-mpegurl" });
		const href = URL.createObjectURL(blob);
		const a = document.createElement("a");
		a.href = href;
		a.download = "stream.m3u";
		document.body.appendChild(a);
		a.click();
		a.remove();
		setTimeout(() => URL.revokeObjectURL(href), 1000);
	}

	const externalMenuItems = $derived<PopoverMenuEntry[]>([
		...(externalUrl
			? [
					{ kind: "header" as const, label: "External player" },
					{
						kind: "item" as const,
						label: "Open playlist (.m3u)",
						onclick: downloadM3u,
					},
					{
						kind: "item" as const,
						label: copied ? "Copied!" : "Copy stream link",
						onclick: copyExternalUrl,
					},
				]
			: []),
		...(onReveal
			? [
					{ kind: "header" as const, label: "On this server" },
					{
						kind: "item" as const,
						label: "Reveal in file explorer",
						onclick: () => onReveal?.(),
					},
				]
			: []),
	]);
</script>

<div
	class="pc"
	style:--accent={accent ? `rgb(${accent})` : "#e4e4e7"}
	style:--accent-dim={accent
		? `rgba(${accent}, 0.5)`
		: "rgba(228, 228, 231, 0.4)"}
>
	<div class="gradient"></div>

	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="progress-container"
		bind:this={trackEl}
		onpointerdown={onPointerDown}
		onpointermove={onPointerMove}
		onpointerup={onPointerUp}
		onpointerleave={onPointerLeave}
		role="slider"
		aria-valuenow={currentTime}
		aria-valuemin={0}
		aria-valuemax={duration}
		tabindex="-1"
	>
		<div class="progress-track" class:segmented={showChapters}>
			{#if pieceMap.length > 0 && streamStats && !streamStats.finished}
				<div class="progress-pieces">
					{#each pieceMap as value}
						<div class="piece" style="opacity: {value / 255}"></div>
					{/each}
				</div>
			{/if}
			{#if showChapters}
				{#each chapters as chapter}
					<div
						class="seg"
						style="left: {(chapter.start / duration) *
							100}%; width: {((chapter.end - chapter.start) / duration) * 100}%"
					>
						<div class="seg-bg">
							<div
								class="seg-buffered"
								style="width: {segFraction(chapter, buffered) * 100}%"
							></div>
							<div
								class="seg-fill"
								style="width: {segFraction(chapter, displayTime) * 100}%"
							></div>
						</div>
					</div>
				{/each}
			{:else}
				<div class="progress-buffered" style="width: {bufferedPercent}%"></div>
				<div class="progress-fill" style="width: {progressPercent}%"></div>
			{/if}
			<div class="progress-thumb" style="left: {progressPercent}%"></div>
		</div>
		{#if showChapters && hovering && hoverChapter}
			<div class="chapter-tooltip" style="left: {hoverPercent}%">
				<span class="chapter-tooltip-title">{hoverChapter.title}</span>
				<span class="chapter-tooltip-time">{formatTime(hoverTime)}</span>
			</div>
		{/if}
	</div>

	<div class="controls-bar">
		<div class="controls-left">
			<Button
				variant="ghost"
				icon={paused ? "Play" : "Pause"}
				{loading}
				onclick={onTogglePlay}
			/>

			<div class="volume-group">
				<Button
					variant="ghost"
					icon={muted || volume === 0
						? "VolumeX"
						: volume < 0.5
							? "Volume1"
							: "Volume2"}
					onclick={onToggleMute}
				/>
				<input
					type="range"
					min="0"
					max="1"
					step="0.01"
					value={muted ? 0 : volume}
					oninput={(e) => onSetVolume(parseFloat(e.currentTarget.value))}
					class="volume-slider"
					class:open={volumeAlwaysOpen}
				/>
			</div>

			<span class="time">
				{formatTime(displayTime)} / {formatTime(duration)}
			</span>
		</div>

		{#if currentChapter}
			<span class="current-chapter" title={currentChapter.title}>
				{currentChapter.title}
			</span>
		{/if}

		<div class="controls-right">
			{#if externalUrl || onReveal}
				<PopoverMenu items={externalMenuItems} align="right">
					{#snippet trigger()}
						<Button variant="ghost" icon="ExternalLink" />
					{/snippet}
				</PopoverMenu>
			{/if}

			{#if streams.length > 0 && onStreamSelect}
				<PopoverMenu items={streamMenuItems} align="right">
					{#snippet trigger()}
						<Button variant="ghost" icon="Settings2" />
					{/snippet}
				</PopoverMenu>
			{/if}

			{#if audioTracks.length > 1 || subtitleTracks.length > 0}
				<PopoverMenu items={audioSubtitleMenuItems} align="right">
					{#snippet trigger()}
						<Button
							variant="ghost"
							icon="ClosedCaption"
							loading={loadingSubtitles}
						/>
					{/snippet}
				</PopoverMenu>
			{/if}

			{#if onToggleFullscreen}
				<Button
					variant="ghost"
					icon={isFullscreen ? "Minimize" : "Maximize"}
					onclick={onToggleFullscreen}
				/>
			{/if}

			{@render rightExtra?.()}
		</div>
	</div>
</div>

<style>
	.pc {
		position: relative;
		width: 100%;
	}

	.gradient {
		position: absolute;
		bottom: 0;
		left: 0;
		right: 0;
		height: 140px;
		background: linear-gradient(transparent, rgba(0, 0, 0, 0.85));
		pointer-events: none;
	}

	/* ── Progress ── */
	.progress-container {
		position: relative;
		height: 20px;
		padding: 7px 0;
		cursor: pointer;
		margin: 0 12px;
		z-index: 1;
		outline: none;
		touch-action: none;
	}

	.progress-track {
		position: relative;
		height: 4px;
		background: rgba(255, 255, 255, 0.15);
		border-radius: 2px;
		overflow: visible;
		transition: height 0.15s ease;
	}

	.progress-container:hover .progress-track {
		height: 6px;
	}

	/* In chapter mode each segment paints its own background; the gaps between
	   them should read as empty rather than the continuous track fill. */
	.progress-track.segmented {
		background: transparent;
	}

	.seg {
		position: absolute;
		top: 0;
		height: 100%;
	}

	.seg-bg {
		position: absolute;
		inset: 0;
		margin-right: 2px; /* the gap between chapters */
		background: rgba(255, 255, 255, 0.15);
		border-radius: 2px;
		overflow: hidden;
	}

	.seg:last-child .seg-bg {
		margin-right: 0;
	}

	.seg-buffered,
	.seg-fill {
		position: absolute;
		top: 0;
		left: 0;
		height: 100%;
	}

	.seg-buffered {
		background: rgba(255, 255, 255, 0.2);
	}

	.seg-fill {
		background: var(--accent);
		transition: width 0.05s linear;
	}

	.progress-pieces {
		position: absolute;
		top: 0;
		left: 0;
		width: 100%;
		height: 100%;
		display: flex;
		border-radius: 2px;
		overflow: hidden;
	}

	.progress-pieces .piece {
		flex: 1;
		background: rgba(255, 255, 255, 0.25);
		transition: opacity 2s ease;
	}

	.progress-buffered {
		position: absolute;
		top: 0;
		left: 0;
		height: 100%;
		background: rgba(255, 255, 255, 0.2);
		border-radius: 2px;
		transition: width 0.1s linear;
	}

	.progress-fill {
		position: absolute;
		top: 0;
		left: 0;
		height: 100%;
		background: var(--accent);
		border-radius: 2px;
		transition: width 0.05s linear;
	}

	.progress-thumb {
		position: absolute;
		top: 50%;
		width: 14px;
		height: 14px;
		background: var(--accent);
		border-radius: 50%;
		transform: translate(-50%, -50%) scale(0);
		transition: transform 0.15s ease;
		box-shadow: 0 0 6px rgba(0, 0, 0, 0.5);
		pointer-events: none;
		z-index: 3;
	}

	.progress-container:hover .progress-thumb {
		transform: translate(-50%, -50%) scale(1);
	}

	.chapter-tooltip {
		position: absolute;
		bottom: 18px;
		transform: translateX(-50%);
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 1px;
		padding: 4px 8px;
		background: rgba(0, 0, 0, 0.85);
		border-radius: 6px;
		pointer-events: none;
		white-space: nowrap;
		z-index: 3;
	}

	.chapter-tooltip-title {
		color: #fff;
		font-size: 12px;
		font-weight: 500;
	}

	.chapter-tooltip-time {
		color: rgba(255, 255, 255, 0.6);
		font-size: 11px;
		font-variant-numeric: tabular-nums;
	}

	.current-chapter {
		position: absolute;
		left: 50%;
		top: 50%;
		transform: translate(-50%, -50%);
		color: rgba(255, 255, 255, 0.65);
		font-size: 13px;
		font-weight: 500;
		max-width: 40%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		text-shadow: 0 1px 4px rgba(0, 0, 0, 0.6);
		pointer-events: none;
	}

	/* ── Controls bar ── */
	.controls-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 4px 12px 10px;
		position: relative;
		z-index: 1;
	}

	.controls-left,
	.controls-right {
		display: flex;
		align-items: center;
		gap: 4px;
	}

	/* ── Volume ── */
	.volume-group {
		display: flex;
		align-items: center;
		gap: 2px;
	}

	.volume-slider {
		width: 0;
		opacity: 0;
		transition:
			width 0.2s ease,
			opacity 0.2s ease;
		accent-color: var(--accent);
		height: 4px;
		cursor: pointer;
		appearance: none;
		-webkit-appearance: none;
		background: transparent;
	}

	.volume-group:hover .volume-slider,
	.volume-slider:focus,
	.volume-slider.open {
		width: 70px;
		opacity: 1;
	}

	.volume-slider::-webkit-slider-runnable-track {
		height: 4px;
		background: rgba(255, 255, 255, 0.2);
		border-radius: 2px;
	}

	.volume-slider::-webkit-slider-thumb {
		-webkit-appearance: none;
		appearance: none;
		width: 12px;
		height: 12px;
		border-radius: 50%;
		background: var(--accent);
		margin-top: -4px;
		cursor: pointer;
	}

	.volume-slider::-moz-range-track {
		height: 4px;
		background: rgba(255, 255, 255, 0.2);
		border-radius: 2px;
		border: none;
	}

	.volume-slider::-moz-range-thumb {
		width: 12px;
		height: 12px;
		border-radius: 50%;
		background: var(--accent);
		border: none;
		cursor: pointer;
	}

	/* ── Time ── */
	.time {
		font-family: "JetBrains Mono", monospace;
		font-size: 0.75rem;
		font-weight: 400;
		letter-spacing: 0.02em;
		margin-left: 8px;
		white-space: nowrap;
	}
</style>
