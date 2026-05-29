<script lang="ts">
	import GradientOverlay from "./GradientOverlay.svelte";
	import { onDestroy } from "svelte";

	let {
		images = [],
		interval = 10000,
		overlay = true,
		override,
		position = "center center",
		dominantColor = $bindable("9, 10, 19"),
		accentColor = $bindable("228, 228, 231"),
	}: {
		images: string[];
		interval?: number;
		overlay?: boolean;
		override?: string;
		position?: string;
		dominantColor?: string;
		accentColor?: string;
	} = $props();

	let layerAEl: HTMLDivElement;
	let layerBEl: HTMLDivElement;
	let overrideAEl: HTMLDivElement;
	let overrideBEl: HTMLDivElement;
	let useA = true;
	let useOverrideA = true;
	let index = 0;
	let timer: ReturnType<typeof setInterval> | null = null;
	let imagesKey = "";
	let baseDark = "9, 10, 19";
	let baseVibrant = "228, 228, 231";

	function preloadImg(url: string): Promise<HTMLImageElement> {
		return new Promise((resolve, reject) => {
			const img = new Image();
			img.crossOrigin = "anonymous";
			img.onload = () => resolve(img);
			img.onerror = () => reject();
			img.src = url;
		});
	}

	function extractColor(img: HTMLImageElement): {
		dark: string;
		vibrant: string;
	} {
		try {
			const canvas = document.createElement("canvas");
			canvas.width = 32;
			canvas.height = 32;
			const ctx = canvas.getContext("2d");
			if (!ctx) return { dark: "9, 10, 19", vibrant: "228, 228, 231" };
			ctx.drawImage(img, 0, 0, 32, 32);
			const data = ctx.getImageData(0, 0, 32, 32).data;
			let r = 0,
				g = 0,
				b = 0,
				count = 0;
			let vr = 0,
				vg = 0,
				vb = 0,
				vcount = 0;
			for (let i = 0; i < data.length; i += 4) {
				const br = data[i] + data[i + 1] + data[i + 2];
				if (br > 60 && br < 600) {
					r += data[i];
					g += data[i + 1];
					b += data[i + 2];
					count++;
				}
				// For vibrant: pick saturated pixels
				const max = Math.max(data[i], data[i + 1], data[i + 2]);
				const min = Math.min(data[i], data[i + 1], data[i + 2]);
				const sat = max > 0 ? (max - min) / max : 0;
				if (sat > 0.2 && br > 80 && br < 600) {
					vr += data[i];
					vg += data[i + 1];
					vb += data[i + 2];
					vcount++;
				}
			}
			const d = 0.35;
			const dark = count
				? `${Math.round((r / count) * d)}, ${Math.round((g / count) * d)}, ${Math.round((b / count) * d)}`
				: "9, 10, 19";
			const vibrant = vcount
				? `${Math.round(vr / vcount)}, ${Math.round(vg / vcount)}, ${Math.round(vb / vcount)}`
				: "228, 228, 231";
			return { dark, vibrant };
		} catch {
			return { dark: "9, 10, 19", vibrant: "228, 228, 231" };
		}
	}

	// Direct DOM crossfade — bypasses Svelte batching entirely
	async function crossfadeTo(url: string) {
		if (!layerAEl || !layerBEl) return;
		try {
			const img = await preloadImg(url);
			const { dark, vibrant } = extractColor(img);
			baseDark = dark;
			baseVibrant = vibrant;
			queueMicrotask(() => {
				dominantColor = dark;
				accentColor = vibrant;
			});
		} catch {}

		const incoming = useA ? layerAEl : layerBEl;
		const outgoing = useA ? layerBEl : layerAEl;

		// 1. Set URL on hidden layer
		incoming.style.backgroundImage = `url(${url})`;

		// 2. Force browser to acknowledge the style before transitioning
		incoming.offsetHeight;

		// 3. Fade in new, fade out old
		incoming.style.opacity = "1";
		outgoing.style.opacity = "0";

		useA = !useA;
	}

	function stopCycling() {
		if (timer) {
			clearInterval(timer);
			timer = null;
		}
	}

	function startCycling() {
		stopCycling();
		if (images.length <= 1) return;
		timer = setInterval(() => {
			if (
				overrideAEl?.style.opacity === "1" ||
				overrideBEl?.style.opacity === "1"
			)
				return;
			index = (index + 1) % images.length;
			crossfadeTo(images[index]);
		}, interval);
	}

	// Watch images
	$effect(() => {
		const key = images.join("\n");
		if (key === imagesKey) return;
		imagesKey = key;
		stopCycling();
		if (!images.length) return;
		index = 0;
		crossfadeTo(images[0]);
		startCycling();
	});

	// Watch override — crossfade between two override layers
	$effect(() => {
		if (!overrideAEl || !overrideBEl) return;
		if (override) {
			preloadImg(override)
				.then((img) => {
					const { dark, vibrant } = extractColor(img);
					queueMicrotask(() => {
						dominantColor = dark;
						accentColor = vibrant;
					});

					const incoming = useOverrideA ? overrideAEl : overrideBEl;
					const outgoing = useOverrideA ? overrideBEl : overrideAEl;

					incoming.style.backgroundImage = `url(${override})`;
					incoming.offsetHeight;
					incoming.style.opacity = "1";
					outgoing.style.opacity = "0";
					useOverrideA = !useOverrideA;
				})
				.catch(() => {});
		} else {
			overrideAEl.style.opacity = "0";
			overrideBEl.style.opacity = "0";
			// Restore base image colors
			queueMicrotask(() => {
				dominantColor = baseDark;
				accentColor = baseVibrant;
			});
		}
	});

	// Reactively update pan position on all layers
	$effect(() => {
		for (const el of [layerAEl, layerBEl, overrideAEl, overrideBEl]) {
			if (el) el.style.transform = `translateX(${position})`;
		}
	});

	onDestroy(stopCycling);
</script>

<div class="cycling-backdrop">
	<div class="layer" bind:this={layerAEl}></div>
	<div class="layer" bind:this={layerBEl}></div>
	<div class="layer override" bind:this={overrideAEl}></div>
	<div class="layer override" bind:this={overrideBEl}></div>
	<GradientOverlay visible={overlay} />
</div>

<style>
	.cycling-backdrop {
		position: absolute;
		inset: 0;
		overflow: hidden;
	}

	.layer {
		position: absolute;
		top: 0;
		left: -15%;
		width: 130%;
		height: 100%;
		background-size: auto 100%;
		background-position: center center;
		opacity: 0;
		transition:
			opacity 1.5s ease,
			transform 0.5s cubic-bezier(0.4, 0, 0.2, 1);
	}

	.override {
		z-index: 1;
	}

	@media (max-width: 768px) {
		.layer {
			transform: none !important;
		}
	}
</style>
