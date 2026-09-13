<script lang="ts">
	import SeasonBrowser from "$lib/components/SeasonBrowser.svelte";
	import { getTitleContext } from "../context";

	const title = getTitleContext();
</script>

{#if title.item?.seasons?.length}
	<div class="browser">
		<SeasonBrowser
			seasons={title.item.seasons}
			onscrollseason={(n) => title.replace(n)}
			onselectepisode={(s, e) => title.go(s, e)}
		/>
	</div>
{/if}

<style>
	.browser {
		display: flex;
		height: 100%;
	}

	@media (max-width: 768px) {
		.browser {
			flex-direction: column;
		}
	}
</style>
