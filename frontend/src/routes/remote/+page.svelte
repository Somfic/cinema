<script lang="ts">
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

	<div class="body">
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
	@use "glow/src/lib/style/theme.scss" as *;

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
