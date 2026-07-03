<script lang="ts">
	import { onDestroy } from "svelte";
	import { goto } from "$app/navigation";
	import { Button, Icon } from "glow";
	import { remote } from "$lib/remote.svelte";
	import PlayerControls from "$lib/components/PlayerControls.svelte";
	import StreamStatsPopover from "$lib/components/StreamStatsPopover.svelte";
	import Spinner from "$lib/components/Spinner.svelte";

	const tv = $derived(remote.tvState);
	// Offset shown relative to the player's -0.25s baseline, matching the player.
	const offsetDisplay = $derived((tv?.subtitleOffset ?? -0.25) + 0.25);

	// Minimise: keep the TV playing, go back to browsing on the phone.
	function minimise() {
		history.back();
	}
	// Stop on the TV: it leaves the player; return the phone to browsing too.
	function stop() {
		remote.send({ kind: "back" });
		history.back();
	}

	// ── Touch gestures over the now-playing area ──
	// A single tap anywhere toggles play/pause; double-tapping the left/right
	// third seeks ∓10s, and keeping that side tapped accumulates the jump
	// (10s, 20s, 30s…). The mobile equivalent of the in-player spacebar /
	// arrow-key shortcuts. The single-tap action is deferred by one double-tap
	// window so a follow-up tap can upgrade it into a seek instead of pausing.
	const SEEK_STEP = 10;
	const DOUBLE_TAP_MS = 300;
	const STREAK_MS = 700;

	let lastTapAt = 0;
	let lastTapZone: "left" | "mid" | "right" | null = null;
	let tapTimer: ReturnType<typeof setTimeout> | undefined;
	let streakSide: "left" | "right" | null = null;
	let streakTimer: ReturnType<typeof setTimeout> | undefined;
	let seekAccum = 0;

	let seekFlash = $state<{
		dir: "left" | "right";
		amount: number;
		id: number;
	} | null>(null);
	let flashSeq = 0;
	let flashTimer: ReturnType<typeof setTimeout> | undefined;

	let playFlash = $state<{ playing: boolean; id: number } | null>(null);
	let playFlashSeq = 0;
	let playFlashTimer: ReturnType<typeof setTimeout> | undefined;

	function togglePlayback() {
		remote.send({ kind: "play_pause" });
		// tv.paused is the state *before* the toggle; flash the resulting action.
		playFlashSeq += 1;
		playFlash = { playing: tv?.paused ?? true, id: playFlashSeq };
		clearTimeout(playFlashTimer);
		playFlashTimer = setTimeout(() => (playFlash = null), 600);
	}

	function addSeek(dir: "left" | "right") {
		seekAccum += SEEK_STEP;
		remote.send({
			kind: "seek_by",
			seconds: dir === "left" ? -SEEK_STEP : SEEK_STEP,
		});
		flashSeq += 1;
		seekFlash = { dir, amount: seekAccum, id: flashSeq };
		clearTimeout(flashTimer);
		flashTimer = setTimeout(() => (seekFlash = null), 800);
		navigator.vibrate?.(15);
		// Keep tapping the same side to add another step without re-double-tapping.
		streakSide = dir;
		clearTimeout(streakTimer);
		streakTimer = setTimeout(() => (streakSide = null), STREAK_MS);
	}

	function handleTap(e: PointerEvent) {
		const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
		const x = (e.clientX - rect.left) / rect.width;
		const zone: "left" | "mid" | "right" =
			x < 0.3 ? "left" : x > 0.7 ? "right" : "mid";
		const side = zone === "mid" ? null : zone;
		const now = e.timeStamp;

		// Continuing an active streak on the same side → add another step.
		if (side && streakSide === side) {
			clearTimeout(tapTimer);
			addSeek(side);
			lastTapAt = now;
			lastTapZone = zone;
			return;
		}

		// Second tap of a double-tap on a side → start a fresh seek streak.
		if (side && lastTapZone === zone && now - lastTapAt < DOUBLE_TAP_MS) {
			clearTimeout(tapTimer);
			seekAccum = 0;
			addSeek(side);
			lastTapAt = 0;
			lastTapZone = null;
			return;
		}

		// Otherwise it's (so far) a single tap → defer play/pause so a follow-up
		// tap can upgrade it into a double-tap seek.
		streakSide = null;
		clearTimeout(streakTimer);
		clearTimeout(tapTimer);
		tapTimer = setTimeout(togglePlayback, DOUBLE_TAP_MS);
		lastTapAt = now;
		lastTapZone = zone;
	}

	onDestroy(() => {
		clearTimeout(tapTimer);
		clearTimeout(streakTimer);
		clearTimeout(flashTimer);
		clearTimeout(playFlashTimer);
	});

	// Bounce out if we somehow land here without an active remote session.
	$effect(() => {
		if (remote.mode !== "remote") goto("/");
	});
</script>

{#snippet subtitleOffsetControl()}
	<div class="offset">
		<Button
			variant="ghost"
			icon="Minus"
			onclick={() => remote.send({ kind: "set_subtitle_offset", offset: -0.25 })}
		/>
		<span class="offset-value">
			{offsetDisplay > 0 ? "+" : ""}{offsetDisplay.toFixed(1)}s
		</span>
		<Button
			variant="ghost"
			icon="Plus"
			onclick={() => remote.send({ kind: "set_subtitle_offset", offset: 0.25 })}
		/>
	</div>
{/snippet}

<div class="remote-page" style:--art={tv?.poster ? `url(${tv.poster})` : "none"}>
	<div class="art-bg"></div>
	<div class="scrim"></div>

	<div class="bar">
		<Button variant="ghost" icon="ChevronDown" onclick={minimise} />
		<span class="casting">
			<Icon name="Cast" size={14} />
			Casting to {remote.pairedPeer?.label ?? "TV"}
		</span>
		<StreamStatsPopover streamStats={tv?.streamStats} />
		<Button variant="ghost" icon="Square" onclick={stop} tooltip="Stop" />
	</div>

	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="body" onpointerup={handleTap}>
		<div class="now">
			{#if tv?.titleImage}
				<img class="now-logo" src={tv.titleImage} alt={tv?.title ?? ""} />
			{:else}
				<span class="now-title">{tv?.title ?? "Playing"}</span>
			{/if}
			{#if tv?.topline}<span class="now-sub">{tv.topline}</span>{/if}
			{#if tv?.loading}
				<span class="buffering">
					<Spinner size={16} />
					{#if tv?.streamStats && !tv.streamStats.finished}
						{tv.streamStats.download_speed_mbps.toFixed(1)} MB/s · {tv
							.streamStats.peers} peers
					{:else}
						Loading…
					{/if}
				</span>
			{/if}
		</div>

		{#if seekFlash}
			{#key seekFlash.id}
				<div class="seek-flash {seekFlash.dir}">
					<Icon
						name={seekFlash.dir === "left" ? "Rewind" : "FastForward"}
						size={26}
					/>
					<span>{seekFlash.amount}s</span>
				</div>
			{/key}
		{/if}

		{#if playFlash}
			{#key playFlash.id}
				<div class="play-flash">
					<Icon name={playFlash.playing ? "Play" : "Pause"} size={40} fill />
				</div>
			{/key}
		{/if}
	</div>

	<div class="controls">
		<PlayerControls
			currentTime={tv?.currentTime ?? 0}
			duration={tv?.duration ?? 0}
			buffered={tv?.buffered ?? 0}
			paused={tv?.paused ?? true}
			volume={tv?.volume ?? 1}
			muted={tv?.muted ?? false}
			streams={tv?.streams ?? []}
			activeStreamHash={tv?.activeStreamHash}
			audioTracks={tv?.audioTracks ?? []}
			activeAudioTrack={tv?.activeAudioTrack ?? 0}
			subtitleTracks={tv?.subtitleTracks ?? []}
			subtitlesActive={tv?.subtitlesActive ?? false}
			activeTrackUrl={tv?.activeTrackUrl}
			transcoding={tv?.transcoding ?? { enabled: false, onlyAudio: false }}
			streamStats={tv?.streamStats ?? null}
			pieceMap={tv?.pieceMap ?? []}
			volumeAlwaysOpen
			loading={tv?.loading ?? false}
			onTogglePlay={() => remote.send({ kind: "play_pause" })}
			onSeek={(t) => remote.send({ kind: "seek", seconds: t })}
			onSetVolume={(v) => remote.send({ kind: "volume", volume: v })}
			onToggleMute={() => remote.send({ kind: "mute" })}
			onToggleFullscreen={() => remote.send({ kind: "fullscreen" })}
			onStreamSelect={(s) => remote.send({ kind: "select_stream", hash: s.info_hash })}
			onAudioSelect={(t) => remote.send({ kind: "select_audio", audioId: t.id })}
			onSubtitleSelect={(t) =>
				remote.send({ kind: "select_subtitle", subtitleUrl: t.url })}
			onSubtitleOff={() => remote.send({ kind: "subtitle_off" })}
			onTranscodingChange={(enabled, onlyAudio) =>
				remote.send({ kind: "set_transcoding", enabled, onlyAudio })}
			{subtitleOffsetControl}
		/>
	</div>
</div>

<style lang="scss">
	@use "glow/styles/theme" as *;

	.remote-page {
		position: fixed;
		inset: 0;
		z-index: 100;
		display: flex;
		flex-direction: column;
		background: $bg-base;
		color: $fg;
		overflow: hidden;
	}

	.art-bg {
		position: absolute;
		inset: -10%;
		background: var(--art) center / cover no-repeat;
		filter: blur(40px) saturate(140%);
		opacity: 0.5;
		transform: scale(1.1);
	}

	.scrim {
		position: absolute;
		inset: 0;
		background: linear-gradient(
			to bottom,
			rgba(0, 0, 0, 0.4),
			rgba(0, 0, 0, 0.85)
		);
	}

	.bar,
	.body,
	.controls {
		position: relative;
		z-index: 1;
	}

	.bar {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.75rem 1rem;
		padding-top: max(0.75rem, env(safe-area-inset-top));
	}

	.casting {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.35rem;
		font-size: 0.8rem;
		opacity: 0.7;
	}

	.body {
		flex: 1;
		display: flex;
		flex-direction: column;
		padding: 1.5rem;
		min-height: 0;
		// Gesture surface: disable double-tap zoom and tap highlight so seeking
		// feels native.
		touch-action: manipulation;
		user-select: none;
		-webkit-user-select: none;
		-webkit-tap-highlight-color: transparent;
		cursor: pointer;
	}

	.seek-flash {
		position: absolute;
		top: 50%;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.25rem;
		font-size: 0.85rem;
		font-weight: 600;
		pointer-events: none;
		animation: seek-pop 600ms ease-out forwards;
	}

	.seek-flash.left {
		left: 12%;
	}

	.seek-flash.right {
		right: 12%;
	}

	@keyframes seek-pop {
		0% {
			opacity: 0;
			transform: translateY(-50%) scale(0.8);
		}
		25% {
			opacity: 1;
			transform: translateY(-50%) scale(1);
		}
		100% {
			opacity: 0;
			transform: translateY(-50%) scale(1);
		}
	}

	.play-flash {
		position: absolute;
		top: 50%;
		left: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		width: 5rem;
		height: 5rem;
		border-radius: 50%;
		background: rgba(0, 0, 0, 0.45);
		pointer-events: none;
		animation: play-pop 600ms ease-out forwards;
	}

	@keyframes play-pop {
		0% {
			opacity: 0;
			transform: translate(-50%, -50%) scale(0.7);
		}
		25% {
			opacity: 1;
			transform: translate(-50%, -50%) scale(1);
		}
		100% {
			opacity: 0;
			transform: translate(-50%, -50%) scale(1.15);
		}
	}

	.now {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.5rem;
		text-align: center;
	}

	.now-logo {
		max-width: min(70%, 18rem);
		max-height: 5rem;
		object-fit: contain;
		filter: drop-shadow(0 2px 12px rgba(0, 0, 0, 0.6));
	}

	.now-title {
		font-size: 1.5rem;
		font-weight: 700;
	}

	.now-sub {
		font-size: 0.9rem;
		opacity: 0.6;
	}

	.buffering {
		position: absolute;
		left: 50%;
		top: 64%;
		transform: translateX(-50%);
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.8rem;
		font-family: "JetBrains Mono", monospace;
		white-space: nowrap;
		opacity: 0.5;
	}

	.controls {
		padding-bottom: env(safe-area-inset-bottom);
	}

	.offset {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 2px 4px;
	}

	.offset-value {
		font-family: monospace;
		font-size: 0.75rem;
		opacity: 0.7;
		min-width: 3.5em;
		text-align: center;
	}
</style>
