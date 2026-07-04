<script lang="ts">
	import { Button, Icon, Glow } from "glow";
	import { remote } from "$lib/remote.svelte";
</script>

<div class="tv-home">
	<Glow
		colors={["#700000", "#008cff", "#75daff", "#ff0026", "#ff3626"]}
		rotation={52}
		zoom={9}
	>
		<div class="exit">
			<Button
				variant="ghost"
				icon="Monitor"
				onclick={() => remote.leaveTv()}
			>
				Exit TV mode
			</Button>
		</div>

		<div class="content">
			<div class="card">
				<div class="mark">
					<Icon name="Clapperboard" size={64} />
					<span class="brand">cinema</span>
				</div>

				<div class="hint">
					<Icon name="Smartphone" size={18} />
					<span>
						{#if remote.controller}
							Browse on <strong>{remote.controller.label}</strong> to start watching
						{:else}
							Browse on your phone to start watching
						{/if}
					</span>
				</div>
			</div>
		</div>
	</Glow>
</div>

<style lang="scss">
	@use "glow/src/lib/style/theme.scss" as *;

	.tv-home {
		position: fixed;
		inset: 0;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		background: $bg-base;
		color: $fg;
	}

	.exit {
		position: fixed;
		top: 1.5rem;
		right: 1.5rem;
		z-index: 2;
		opacity: 0.55;
		transition: opacity 150ms ease;

		&:hover {
			opacity: 1;
		}
	}

	.content {
		display: flex;
		align-items: center;
		justify-content: center;
		height: 100%;
		padding: 2rem;
	}

	.card {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2rem;
		padding: 3rem 3.5rem;
		max-width: 520px;
		background: rgba(10, 12, 20, 0.55);
		backdrop-filter: blur(12px);
		border: 1px solid rgba(255, 255, 255, 0.12);
		border-radius: 16px;
	}

	.mark {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.75rem;
		opacity: 0.92;
	}

	.brand {
		font-family: $font-family-header;
		font-size: 2.75rem;
		font-weight: 700;
		letter-spacing: 0.02em;
	}

	.hint {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 1rem;
		opacity: 0.7;
		text-align: center;
	}

	.hint strong {
		font-weight: 600;
		opacity: 0.9;
	}
</style>
