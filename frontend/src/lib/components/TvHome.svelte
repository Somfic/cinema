<script lang="ts">
	import { Button, Icon } from "glow";
	import { remote } from "$lib/remote.svelte";
</script>

<div class="tv-home">
	<div class="godrays"></div>

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
		<div class="mark">
			<Icon name="Clapperboard" size={64} />
			<span class="brand">cinema</span>
		</div>
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
		z-index: 1;
		opacity: 0.55;
		transition: opacity 150ms ease;

		&:hover {
			opacity: 1;
		}
	}

	.content {
		position: relative;
		z-index: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2rem;
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
		position: fixed;
		bottom: 2.5rem;
		left: 50%;
		transform: translateX(-50%);
		z-index: 1;
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 1rem;
		opacity: 0.55;
		white-space: nowrap;
	}

	.hint strong {
		font-weight: 600;
		opacity: 0.9;
	}

	.godrays {
		position: fixed;
		width: 100vw;
		height: 100vh;
		top: 0;
		left: 0;
		pointer-events: none;

		--stripes: repeating-linear-gradient(
			100deg,
			#fff 0%,
			#fff 7%,
			transparent 10%,
			transparent 12%,
			#fff 16%
		);
		--stripesDark: repeating-linear-gradient(
			100deg,
			#000 0%,
			#000 7%,
			transparent 10%,
			transparent 12%,
			#000 16%
		);
		--rainbow: repeating-linear-gradient(
			100deg,
			#60a5fa 10%,
			#e879f9 15%,
			#60a5fa 20%,
			#5eead4 25%,
			#60a5fa 30%
		);
		background-image: var(--stripesDark), var(--rainbow);
		background-size: 430%, 300%;
		background-position:
			50% 50%,
			50% 50%;

		filter: blur(12px) invert(100%) saturate(110%);

		mask-image: radial-gradient(ellipse at 100% 0%, black 55%, transparent 90%);

		opacity: 0.3;

		&::after {
			content: "";
			position: absolute;
			inset: 0;
			background-image: var(--stripes), var(--rainbow);
			background-size: 340%, 170%;
			animation: jumbo 80s linear infinite;
			background-attachment: fixed;
			mix-blend-mode: difference;
		}
	}

	@keyframes jumbo {
		from {
			background-position:
				50% 50%,
				50% 50%;
		}
		to {
			background-position:
				350% 50%,
				350% 50%;
		}
	}
</style>
