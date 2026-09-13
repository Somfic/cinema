<script lang="ts">
	import MediaInfo from "$lib/components/MediaInfo.svelte";
	import { getTitleContext } from "./context";

	// Everything here comes from the layout: it owns the fetch, the backdrop and
	// the playback session, all of which have to outlive a move to a season or
	// episode route.
	const title = getTitleContext();
</script>

{#if title.item}
	<div class="info">
		<MediaInfo
			item={title.item}
			loadingStreams={title.loadingStreams}
			resumeEntry={title.resumeEntry}
			playing={false}
			tvMode={false}
			onwatch={title.playMovie}
			onresume={title.resume}
			onselectseason={(s) => title.go(s)}
			onselectepisode={(s, e) => title.go(s, e)}
		/>
	</div>
{/if}

<style>
	/* The hero's text column sits against the right edge, where the backdrop's
	   scrim gradient is — the same framing the slider's info slide had. */
	.info {
		position: relative;
		display: flex;
		justify-content: flex-end;
		height: 100%;
		overflow-y: auto;
	}

	@media (max-width: 768px) {
		.info {
			justify-content: flex-start;
		}
	}
</style>
