/**
 * Is one of Glow's portalled overlays open?
 *
 * Glow used to publish this itself (`isOverlayOpen` / `onOverlayChange`); the
 * update dropped both. The overlays still portal to <body> as direct children
 * with stable class names, so watch for them there instead — a `childList`
 * observer on <body> alone, without `subtree`, so nothing pays for it on every
 * DOM change during playback.
 */
const OVERLAY_SELECTOR = [
	".popover-content",
	".popover-sheet-root",
	".context-menu",
	".modal-overlay",
	".drawer-overlay",
	".cp-overlay",
].join(", ");

let open = $state(false);
let observer: MutationObserver | null = null;
let refs = 0;

function measure() {
	open = !!document.querySelector(OVERLAY_SELECTOR);
}

/**
 * Start watching while the caller is mounted. Returns the teardown, so the
 * whole thing is `$effect(() => trackOverlays())`. Refcounted: several
 * watchers share one observer.
 */
export function trackOverlays(): () => void {
	if (refs++ === 0) {
		observer = new MutationObserver(measure);
		observer.observe(document.body, { childList: true });
	}
	measure();
	return () => {
		if (--refs === 0) {
			observer?.disconnect();
			observer = null;
		}
	};
}

export const overlays = {
	get open() {
		return open;
	},
};
