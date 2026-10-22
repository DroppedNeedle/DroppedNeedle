import { browser } from '$app/environment';

const BLANK_GIF = 'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';

/** How far outside the visible area an image starts loading, in CSS pixels. */
const LAZY_MARGIN_PX = 300;

/**
 * How many leading items of a grid or carousel load their image straight
 * away instead of waiting for the observer: about one row on a wide screen.
 */
export const EAGER_IMAGE_COUNT = 6;

/**
 * Marks a horizontal scroll strip, such as a carousel's. Images inside it are
 * watched against the strip itself, so the ones just past its right edge
 * start loading too.
 */
const LAZY_STRIP_ATTRIBUTE = 'data-lazy-strip';

interface Watcher {
	observe(img: HTMLImageElement): void;
	unobserve(img: HTMLImageElement): void;
	release(): void;
}

const watcherOf = new WeakMap<HTMLImageElement, Watcher>();
const pendingImages: Set<HTMLImageElement> = new Set();

function loadImage(img: HTMLImageElement) {
	watcherOf.get(img)?.unobserve(img);
	const src = img.dataset.src;
	if (!src || img.src === src) return;
	if (img.dataset.srcset) img.srcset = img.dataset.srcset;
	img.src = src;
}

let pageObserver: IntersectionObserver | null = null;
let pageUsers = 0;

const pageWatcher: Watcher = {
	observe: (img) => pageObserver?.observe(img),
	unobserve: (img) => pageObserver?.unobserve(img),
	release() {
		pageUsers--;
		if (pageUsers <= 0 && pageObserver) {
			pageObserver.disconnect();
			pageObserver = null;
			pageUsers = 0;
		}
	}
};

function acquirePageWatcher(): Watcher {
	pageObserver ??= new IntersectionObserver(
		(entries) => {
			for (const entry of entries) {
				if (entry.isIntersecting) loadImage(entry.target as HTMLImageElement);
			}
		},
		{ rootMargin: `${LAZY_MARGIN_PX}px` }
	);
	pageUsers++;
	return pageWatcher;
}

/**
 * Watches the images of one scroll strip. An image loads once it is within
 * the margin of the strip's own box, while the strip is within the margin of
 * the page's visible area.
 */
class StripWatcher implements Watcher {
	#near = false;
	#users = 0;
	#inReach = new Set<HTMLImageElement>();
	#page: IntersectionObserver;
	#strip: IntersectionObserver;
	#onEmpty: () => void;

	constructor(root: HTMLElement, onEmpty: () => void) {
		this.#onEmpty = onEmpty;
		this.#page = new IntersectionObserver(
			(entries) => {
				this.#near = entries[entries.length - 1].isIntersecting;
				this.#flush();
			},
			{ rootMargin: `${LAZY_MARGIN_PX}px` }
		);
		this.#strip = new IntersectionObserver(
			(entries) => {
				for (const entry of entries) {
					const img = entry.target as HTMLImageElement;
					if (entry.isIntersecting) this.#inReach.add(img);
					else this.#inReach.delete(img);
				}
				this.#flush();
			},
			{ root, rootMargin: `0px ${LAZY_MARGIN_PX}px` }
		);
		this.#page.observe(root);
	}

	acquire(): Watcher {
		this.#users++;
		return this;
	}

	observe(img: HTMLImageElement) {
		this.#strip.observe(img);
	}

	unobserve(img: HTMLImageElement) {
		this.#inReach.delete(img);
		this.#strip.unobserve(img);
	}

	release() {
		this.#users--;
		if (this.#users > 0) return;
		this.#page.disconnect();
		this.#strip.disconnect();
		this.#inReach.clear();
		this.#onEmpty();
	}

	#flush() {
		if (!this.#near) return;
		for (const img of [...this.#inReach]) loadImage(img);
	}
}

const strips = new WeakMap<Element, StripWatcher>();

function acquireWatcher(img: HTMLImageElement): Watcher | null {
	if (!browser || typeof IntersectionObserver === 'undefined') return null;
	const root = img.parentElement?.closest<HTMLElement>(`[${LAZY_STRIP_ATTRIBUTE}]`);
	if (!root) return acquirePageWatcher();
	let strip = strips.get(root);
	if (!strip) {
		strip = new StripWatcher(root, () => strips.delete(root));
		strips.set(root, strip);
	}
	return strip.acquire();
}

export function cancelPendingImages() {
	pendingImages.forEach((img) => {
		watcherOf.get(img)?.unobserve(img);
		img.src = BLANK_GIF;
		img.removeAttribute('srcset');
	});
	pendingImages.clear();
}

export function lazyImage(img: HTMLImageElement) {
	const watcher = acquireWatcher(img);

	pendingImages.add(img);

	if (watcher) {
		watcherOf.set(img, watcher);
		watcher.observe(img);
	} else {
		const src = img.dataset.src;
		if (src) {
			if (img.dataset.srcset) img.srcset = img.dataset.srcset;
			img.src = src;
			pendingImages.delete(img);
		}
	}

	const handleLoad = () => pendingImages.delete(img);
	const handleError = () => pendingImages.delete(img);
	img.addEventListener('load', handleLoad);
	img.addEventListener('error', handleError);

	return {
		update() {
			if (watcher) {
				watcher.unobserve(img);
				pendingImages.add(img);
				watcher.observe(img);
			}
		},
		destroy() {
			img.removeEventListener('load', handleLoad);
			img.removeEventListener('error', handleError);
			pendingImages.delete(img);
			if (watcher) {
				watcher.unobserve(img);
				watcherOf.delete(img);
				watcher.release();
			}
		}
	};
}

export function resetLazyImage(img: HTMLImageElement, newSrc: string, newSrcset?: string) {
	img.classList.add('opacity-0');
	img.src = BLANK_GIF;
	img.removeAttribute('srcset');
	img.dataset.src = newSrc;
	if (newSrcset) img.dataset.srcset = newSrcset;
	else delete img.dataset.srcset;

	const watcher = watcherOf.get(img);
	if (watcher) {
		watcher.unobserve(img);
		pendingImages.add(img);
		watcher.observe(img);
	}
}
