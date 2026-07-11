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
		palette = $bindable<string[]>([]),
	}: {
		images: string[];
		interval?: number;
		overlay?: boolean;
		override?: string;
		position?: string;
		dominantColor?: string;
		accentColor?: string;
		palette?: string[];
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
	let basePalette: string[] = [];

	function preloadImg(url: string): Promise<HTMLImageElement> {
		return new Promise((resolve, reject) => {
			const img = new Image();
			img.crossOrigin = "anonymous";
			img.onload = () => resolve(img);
			img.onerror = () => reject();
			img.src = url;
		});
	}

	// One quantized histogram bucket of similar pixels.
	type Bucket = { r: number; g: number; b: number; n: number };
	// A bucket's representative color plus its HSL saturation/luminance.
	type Cand = { r: number; g: number; b: number; n: number; s: number; l: number };

	function extractColor(img: HTMLImageElement): {
		dark: string;
		vibrant: string;
		palette: string[];
	} {
		const FALLBACK = {
			dark: "9, 10, 19",
			vibrant: "228, 228, 231",
			palette: [] as string[],
		};
		try {
			const canvas = document.createElement("canvas");
			canvas.width = 32;
			canvas.height = 32;
			const ctx = canvas.getContext("2d");
			if (!ctx) return FALLBACK;
			ctx.drawImage(img, 0, 0, 32, 32);
			const data = ctx.getImageData(0, 0, 32, 32).data;

			// Single pass: accumulate the overall dark average (the deep base for
			// the glow gap — kept from the old extractor so text stays readable
			// over it) plus a 5-bit-per-channel color histogram used to pull out
			// distinct swatches below.
			let r = 0,
				g = 0,
				b = 0,
				count = 0;
			const buckets = new Map<number, Bucket>();
			let maxCount = 0;
			for (let i = 0; i < data.length; i += 4) {
				const pr = data[i],
					pg = data[i + 1],
					pb = data[i + 2];
				const br = pr + pg + pb;
				if (br > 60 && br < 600) {
					r += pr;
					g += pg;
					b += pb;
					count++;
				}
				const key = ((pr >> 3) << 10) | ((pg >> 3) << 5) | (pb >> 3);
				let bucket = buckets.get(key);
				if (!bucket) {
					bucket = { r: 0, g: 0, b: 0, n: 0 };
					buckets.set(key, bucket);
				}
				bucket.r += pr;
				bucket.g += pg;
				bucket.b += pb;
				bucket.n++;
				if (bucket.n > maxCount) maxCount = bucket.n;
			}

			const d = 0.35;
			const dark = count
				? `${Math.round((r / count) * d)}, ${Math.round((g / count) * d)}, ${Math.round((b / count) * d)}`
				: FALLBACK.dark;

			// Reduce each bucket to its average color + HSL saturation/luminance.
			const cands: Cand[] = [];
			for (const bk of buckets.values()) {
				const cr = bk.r / bk.n,
					cg = bk.g / bk.n,
					cb = bk.b / bk.n;
				const mx = Math.max(cr, cg, cb) / 255,
					mn = Math.min(cr, cg, cb) / 255;
				const l = (mx + mn) / 2;
				const s = mx === mn ? 0 : (mx - mn) / (1 - Math.abs(2 * l - 1));
				cands.push({ r: cr, g: cg, b: cb, n: bk.n, s, l });
			}

			// Vibrant-style extraction: for each (luminance × saturation) target,
			// pick the best-scoring bucket. This gives a poster's distinct hues
			// their own stop instead of averaging them into mud. Score weights
			// (Vibrant.js defaults): saturation 3, luminance 6.5, population 0.5.
			const WS = 3,
				WL = 6.5,
				WP = 0.5,
				WT = WS + WL + WP;
			const targets = [
				{ tl: 0.26, minl: 0, maxl: 0.45, ts: 1, mins: 0.35, maxs: 1 }, // dark vibrant
				{ tl: 0.5, minl: 0.3, maxl: 0.7, ts: 1, mins: 0.35, maxs: 1 }, // vibrant
				{ tl: 0.74, minl: 0.55, maxl: 1, ts: 1, mins: 0.35, maxs: 1 }, // light vibrant
				{ tl: 0.26, minl: 0, maxl: 0.45, ts: 0.3, mins: 0, maxs: 0.4 }, // dark muted
				{ tl: 0.5, minl: 0.3, maxl: 0.7, ts: 0.3, mins: 0, maxs: 0.4 }, // muted
				{ tl: 0.74, minl: 0.55, maxl: 1, ts: 0.3, mins: 0, maxs: 0.4 }, // light muted
			];
			const used = new Set<Cand>();
			const picked: Cand[] = [];
			let vibrantCand: Cand | null = null;
			for (let t = 0; t < targets.length; t++) {
				const tg = targets[t];
				let best: Cand | null = null;
				let bestScore = -1;
				for (const c of cands) {
					if (used.has(c)) continue;
					if (c.s < tg.mins || c.s > tg.maxs) continue;
					if (c.l < tg.minl || c.l > tg.maxl) continue;
					const score =
						(WS * (1 - Math.abs(c.s - tg.ts)) +
							WL * (1 - Math.abs(c.l - tg.tl)) +
							WP * (c.n / maxCount)) /
						WT;
					if (score > bestScore) {
						bestScore = score;
						best = c;
					}
				}
				if (best) {
					used.add(best);
					picked.push(best);
					if (t === 1) vibrantCand = best; // the "Vibrant" swatch
				}
			}

			// Accent: prefer the true Vibrant swatch, else the most saturated pick.
			const accent =
				vibrantCand ??
				picked.slice().sort((a, b) => b.s - a.s)[0] ??
				null;
			const vibrant = accent
				? `${Math.round(accent.r)}, ${Math.round(accent.g)}, ${Math.round(accent.b)}`
				: FALLBACK.vibrant;

			// Order the swatches dark → light for the glow ramp, drop near-
			// duplicates, and cap at Glow's 5-stop limit.
			const palette: string[] = [];
			let prev: Cand | null = null;
			for (const c of picked.slice().sort((a, b) => a.l - b.l)) {
				if (prev) {
					const dr = c.r - prev.r,
						dg = c.g - prev.g,
						db = c.b - prev.b;
					if (dr * dr + dg * dg + db * db < 900) continue; // < ~30 apart
				}
				palette.push(
					`${Math.round(c.r)}, ${Math.round(c.g)}, ${Math.round(c.b)}`,
				);
				prev = c;
				if (palette.length >= 5) break;
			}

			return { dark, vibrant, palette };
		} catch {
			return FALLBACK;
		}
	}

	// Direct DOM crossfade — bypasses Svelte batching entirely
	async function crossfadeTo(url: string) {
		if (!layerAEl || !layerBEl) return;
		try {
			const img = await preloadImg(url);
			const { dark, vibrant, palette: pal } = extractColor(img);
			baseDark = dark;
			baseVibrant = vibrant;
			basePalette = pal;
			queueMicrotask(() => {
				dominantColor = dark;
				accentColor = vibrant;
				palette = pal;
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
					const { dark, vibrant, palette: pal } = extractColor(img);
					queueMicrotask(() => {
						dominantColor = dark;
						accentColor = vibrant;
						palette = pal;
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
				palette = basePalette;
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
		/* `auto 100%` so the image always spans the full height and is never
		   vertically zoomed/cropped — width follows aspect ratio (we don't care
		   about horizontal coverage; the edge mask feathers any gap). */
		background-size: auto 100%;
		background-position: center center;
		background-repeat: no-repeat;
		/* Feather the left/right edges so the image fades softly into the dark
		   background near the screen edges instead of ending hard. */
		mask-image: linear-gradient(
			to right,
			transparent 3%,
			#000 18%,
			#000 82%,
			transparent 97%
		);
		-webkit-mask-image: linear-gradient(
			to right,
			transparent 3%,
			#000 18%,
			#000 82%,
			transparent 97%
		);
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
