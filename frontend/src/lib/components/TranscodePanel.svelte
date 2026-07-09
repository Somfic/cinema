<script lang="ts">
	import type { AudioTrack, Download, Pretranscoding } from "$lib/schema";
	import { api } from "$lib/api";
	import { PRETRANSCODING_STATUS_LABEL, pretranscodePercent } from "$lib/utils";
	import { Button, RadioInput, Text, toast, tooltip } from "glow";

	let {
		download,
		pretranscodings = [],
		active = false,
	}: {
		download: Download;
		pretranscodings?: Pretranscoding[];
		active?: boolean;
	} = $props();

	// Selection state for the "start a new job" form.
	let onlyAudio = $state(true);
	let audioIndex = $state<number | null>(null);
	let starting = $state(false);

	// Audio tracks are lazy-loaded on first activation; probing them
	// round-trips through the torrent stream reader, which can be slow.
	let audioTracks = $state<AudioTrack[] | null>(null);
	let loadingTracks = $state(false);

	$effect(() => {
		if (!active || audioTracks !== null || loadingTracks) return;
		loadingTracks = true;
		api.streams
			.audioTracks(download.info_hash, download.file_idx)
			.then((probe) => {
				audioTracks = probe.tracks;
				if (audioIndex === null && probe.tracks.length > 0) {
					audioIndex = probe.tracks[0].stream_index;
				}
			})
			.catch((err) => {
				const msg = err instanceof Error ? err.message : String(err);
				toast.error(`Could not load audio tracks: ${msg}`);
			})
			.finally(() => {
				loadingTracks = false;
			});
	});

	async function start() {
		if (audioIndex === null) return;
		try {
			starting = true;
			await api.transcodings.enqueue({
				download_id: download.id,
				only_audio: onlyAudio,
				audio_index: audioIndex,
			});
			toast.success("Pretranscoding queued");
		} catch (err: unknown) {
			toast.error(
				`Enqueue failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		} finally {
			starting = false;
		}
	}

	async function cancel(pt: Pretranscoding) {
		try {
			await api.transcodings.cancel(pt.id);
		} catch (err: unknown) {
			toast.error(
				`Cancel failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async function pause(pt: Pretranscoding) {
		try {
			await api.transcodings.pause(pt.id);
		} catch (err: unknown) {
			toast.error(
				`Pause failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async function resume(pt: Pretranscoding) {
		try {
			await api.transcodings.resume(pt.id);
		} catch (err: unknown) {
			toast.error(
				`Resume failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	async function remove(pt: Pretranscoding) {
		try {
			await api.transcodings.remove(pt.id);
		} catch (err: unknown) {
			toast.error(
				`Remove failed: ${err instanceof Error ? err.message : String(err)}`,
			);
		}
	}

	function trackLabel(index: number): string {
		const track = audioTracks?.find((t) => t.stream_index === index);
		if (!track) return `Track ${index}`;
		if (track.language) return `${track.name} (${track.language})`;
		return track.name;
	}
</script>

<div class="panel">
	{#if pretranscodings.length > 0}
		<div class="section">
			<Text size="xs" variant="muted">Cached copies</Text>
			{#each pretranscodings as pt (pt.id)}
				{@const pct = pretranscodePercent(pt)}
				<div class="row">
					<div class="row-info">
						<div class="row-title">
							<Text size="sm">
								{pt.only_audio ? "Only audio" : "Full"} · {trackLabel(
									pt.audio_index,
								)}
							</Text>
							<span
								class="chip"
								class:chip--transcoding={pt.status === "Transcoding"}
								class:chip--queued={pt.status === "Queued"}
								class:chip--paused={pt.status === "Paused"}
								class:chip--done={pt.status === "Completed"}
								class:chip--failed={pt.status === "Failed"}
							>
								{PRETRANSCODING_STATUS_LABEL[pt.status]}
							</span>
						</div>
						{#if pt.status === "Transcoding" || pt.status === "Paused"}
							<div class="bar-wrap">
								<div
									class="bar"
									class:bar--paused={pt.status === "Paused"}
									style={pct != null
										? `width: ${pct}%`
										: "width: 100%; opacity: 0.4"}
								></div>
							</div>
							<Text size="xs" variant="muted">
								{pct != null ? `${pct}%` : "Waiting for pieces…"}
							</Text>
						{:else if pt.status === "Failed" && pt.error}
							<Text size="xs" variant="muted">{pt.error}</Text>
						{/if}
					</div>
					<div class="row-actions">
						{#if pt.status === "Transcoding" || pt.status === "Queued"}
							<Button icon="Pause" variant="ghost" onclick={() => pause(pt)} />
							<Button icon="X" variant="ghost" onclick={() => cancel(pt)} />
						{:else if pt.status === "Paused"}
							<Button icon="Play" variant="ghost" onclick={() => resume(pt)} />
							<Button icon="X" variant="ghost" onclick={() => cancel(pt)} />
						{:else}
							<Button icon="Trash" variant="ghost" onclick={() => remove(pt)} />
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}

	<div class="section">
		<Text size="xs" variant="muted">New pretranscoding</Text>
		<div class="pretranscoding-options">
			<RadioInput
				iconOnly
				value={onlyAudio ? "only-audio" : "full"}
				options={[
					{
						label: "Only Audio",
						icon: "AudioLines",
						value: "only-audio",
					},
					{
						label: "Full",
						icon: "Film",
						value: "full",
					},
				]}
				onChange={(value) => {
					onlyAudio = value !== "full";
				}}
			/>
			<div class="track-select">
				{#if loadingTracks}
					<Text size="xs" variant="muted">Loading…</Text>
				{:else if audioTracks && audioTracks.length > 0}
					<select
						bind:value={audioIndex}
						use:tooltip={{ content: "Audio track", delay: 500 }}
					>
						{#each audioTracks as track (track.stream_index)}
							<option value={track.stream_index}>
								{track.name}{track.language ? ` (${track.language})` : ""}
							</option>
						{/each}
					</select>
				{:else if audioTracks !== null}
					<Text size="xs" variant="muted">No audio tracks detected</Text>
				{:else}
					<Text size="xs" variant="muted">Loading…</Text>
				{/if}
			</div>
		</div>

		<Button
			variant="primary"
			disabled={audioIndex === null || starting}
			onclick={start}
		>
			Start pretranscoding
		</Button>
	</div>
</div>

<style lang="scss">
	@use "glow/styles/theme" as *;

	.panel {
		display: flex;
		flex-direction: column;
		background: rgba(255, 255, 255, 0.03);
		border-radius: 6px;
		border: $border;
	}

	.section {
		padding: 0.6rem 0.75rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;

		& + .section {
			border-top: $border;
		}
	}

	.row {
		display: flex;
		gap: 0.5rem;
		align-items: center;
	}

	.row-info {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.row-title {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex-wrap: wrap;
	}

	.row-actions {
		display: flex;
		gap: 0.1rem;
		flex-shrink: 0;
	}

	.chip {
		font-size: 0.65rem;
		padding: 0.1rem 0.35rem;
		border-radius: 4px;
		background: rgba(255, 255, 255, 0.08);
		white-space: nowrap;
	}
	.chip--transcoding {
		background: rgba(59, 130, 246, 0.2);
		color: rgb(100, 170, 255);
	}
	.chip--queued {
		background: rgba(255, 255, 255, 0.1);
	}
	.chip--paused {
		background: rgba(234, 179, 8, 0.2);
		color: rgb(234, 179, 8);
	}
	.chip--done {
		background: rgba(34, 197, 94, 0.2);
		color: rgb(34, 197, 94);
	}
	.chip--failed {
		background: rgba(239, 68, 68, 0.2);
		color: rgb(239, 68, 68);
	}

	.bar-wrap {
		height: 3px;
		border-radius: 2px;
		background: rgba(255, 255, 255, 0.1);
		overflow: hidden;
	}

	.bar {
		height: 100%;
		background: $primary;
		border-radius: 2px;
		transition: width 0.5s;
	}
	.bar--paused {
		background: rgb(234, 179, 8);
	}

	.pretranscoding-options {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}

	.track-select select {
		flex: 1;
		max-width: 12rem;
		background: rgba(255, 255, 255, 0.05);
		color: inherit;
		border: $border;
		border-radius: 4px;
		padding: 0.15rem 0.35rem;
		font-size: 0.8rem;

		option {
			background-color: $bg-surface-element;
		}
	}
</style>
