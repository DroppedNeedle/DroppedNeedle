<!--
	The page's dot frame: a small accent at the top-left, a fuller spread in
	the top-right and a fade rising from the bottom edge, densest at the
	corner or edge and thinning to nothing. It fills its nearest positioned
	ancestor, behind that ancestor's content; the bottom fade rises from the
	element in it marked `data-frit-floor` (else from its bottom). The dots
	curve round the page's text, controls, images and icons, so nothing the
	page shows sits on them.
	The dots press in from the corners when the frame mounts (key it on the
	route to print again), and as the page scrolls the top corners ease away
	and the bottom edge rises a little. It draws only then. Once printed it
	breathes: each band is drawn twice, the second time with its fields at
	BREATH_LEAST of their extent, and the full drawing fades out over the
	smaller one and back on a slow cycle. The dots never move, so they keep
	clear of content throughout, and the compositor runs it with no redraw.
	It pauses while the tab is hidden, a band is off screen or the page
	scrolls. Reduced motion gets the finished frame with no drift or breath;
	high contrast and forced colours get none (`dn-decor`).
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { drawFrit, easeOut, PRINT_MS, type FritClear, type FritField } from './frit';

	let root: HTMLDivElement;
	let topCanvas: HTMLCanvasElement;
	let bottomCanvas: HTMLCanvasElement;
	// The same bands with their fields at BREATH_LEAST, shown under the
	// full ones as they fade.
	let topLeast: HTMLCanvasElement;
	let bottomLeast: HTMLCanvasElement;

	// Every size is in CSS pixels, from the frame's own width.
	function sizes(width: number) {
		const phone = width < 640;
		// The top-left stays a sliver at the edge, wider only as the gutter
		// beside the page's 80rem column grows.
		const gutter = Math.max(0, (width - 1280) / 2);
		return {
			step: phone ? 7 : 9,
			left: phone ? null : { rx: Math.min(40 + gutter / 2, 120), ry: 200 },
			// Wider than tall, so it ends above a hero's side column.
			right: phone ? { rx: width * 0.32, ry: 120 } : { rx: Math.min(width * 0.38, 560), ry: 220 },
			// Heaviest at the two sides, thinning toward the middle.
			bottom: { rx: width * 0.38, ry: phone ? 60 : 108 }
		};
	}

	// What the dots keep clear of, besides text: controls, media and badges.
	const CONTENT =
		'a, button, input, select, textarea, img, svg, video, canvas, picture, [role="button"], [role="link"], .badge';

	// The most the top corners shrink and the bottom edge rises, and the
	// scroll over which the corners shrink.
	const SHRINK = 0.35;
	const RISE = 0.3;
	const DRIFT_PX = 900;

	// The breath: 7 s each way, so a 14 s cycle, from the drawn frame down to
	// fields of BREATH_LEAST their extent and back.
	const BREATH_MS = 7000;
	const BREATH_LEAST = 0.86;
	const SCROLL_REST_MS = 200;

	onMount(() => {
		const host = root.parentElement;
		const top = topCanvas.getContext('2d');
		const bottom = bottomCanvas.getContext('2d');
		const topSmall = topLeast.getContext('2d');
		const bottomSmall = bottomLeast.getContext('2d');
		if (!host || !top || !bottom || !topSmall || !bottomSmall) return;

		const quiet = matchMedia('(prefers-contrast: more), (forced-colors: active)');
		const reduce = matchMedia('(prefers-reduced-motion: reduce)');
		const started = performance.now();
		let width = 0;
		let topHeight = 0;
		let bottomHeight = 0;
		let hostTop = 0;
		let floorTop = 0;
		let colour = '';
		let raf = 0;
		// Boxes of page content each canvas keeps clear, in its own pixels.
		// They move with the page, so a scroll never needs them again.
		let clearTop: FritClear[] = [];
		let clearBottom: FritClear[] = [];
		// Counts layouts, so a frame redraws only what changed since the last.
		let layout = 0;
		let drawnTop = '';
		let drawnBottom = '';
		// The two breaths, top then bottom, once the print-in has finished.
		let breaths: Animation[] = [];
		// Whether each canvas, top then bottom, is on screen.
		const shown = [false, false];
		let scrolling = 0;

		function fit(canvas: HTMLCanvasElement, ctx: CanvasRenderingContext2D, height: number) {
			const dpr = Math.min(window.devicePixelRatio || 1, 2);
			canvas.width = Math.round(width * dpr);
			canvas.height = Math.round(height * dpr);
			canvas.style.width = `${width}px`;
			canvas.style.height = `${height}px`;
			ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		}

		// Collects the boxes of what the page shows across one band (viewport
		// pixels): text runs, controls, images and icons, never a layout box.
		// A subtree outside the band is skipped whole, so this stays cheap.
		function collect(bandTop: number, bandBottom: number, left: number): FritClear[] {
			const boxes: FritClear[] = [];
			const area = width * (bandBottom - bandTop);
			const keep = (r: DOMRect) => {
				if (r.width === 0 || r.height === 0 || r.bottom < bandTop || r.top > bandBottom) return;
				boxes.push({
					left: r.left - left,
					top: r.top - bandTop,
					right: r.right - left,
					bottom: r.bottom - bandTop
				});
			};
			const range = document.createRange();
			const visit = (el: Element) => {
				if (el === root) return;
				const r = el.getBoundingClientRect();
				if (r.width === 0 || r.bottom < bandTop || r.top > bandBottom) return;
				if (el.matches(CONTENT) && r.width * r.height < area / 3) {
					keep(r);
					return;
				}
				for (const node of el.childNodes) {
					if (node.nodeType === Node.ELEMENT_NODE) visit(node as Element);
					else if (node.nodeType === Node.TEXT_NODE && node.textContent?.trim()) {
						range.selectNodeContents(node);
						for (const line of range.getClientRects()) keep(line);
					}
				}
			};
			for (const child of host!.children) visit(child);
			return boxes;
		}

		// Reads the page's layout; the canvases change size only with the width.
		function measure() {
			const box = host!.getBoundingClientRect();
			const floor = host!.querySelector('[data-frit-floor]');
			hostTop = box.top + window.scrollY;
			floorTop = floor ? floor.getBoundingClientRect().top - box.top : box.height;
			colour = getComputedStyle(root).color;
			const next = Math.round(box.width);
			if (next !== width) {
				width = next;
				const s = sizes(width);
				topHeight = Math.ceil(Math.max(s.left?.ry ?? 0, s.right.ry));
				bottomHeight = Math.ceil(s.bottom.ry * (1 + RISE));
				fit(topCanvas, top!, topHeight);
				fit(bottomCanvas, bottom!, bottomHeight);
				fit(topLeast, topSmall!, topHeight);
				fit(bottomLeast, bottomSmall!, bottomHeight);
			}
			const bottomAt = Math.max(0, floorTop - bottomHeight);
			bottomCanvas.style.top = `${bottomAt}px`;
			bottomLeast.style.top = `${bottomAt}px`;
			clearTop = collect(box.top, box.top + topHeight, box.left);
			clearBottom = collect(box.top + bottomAt, box.top + bottomAt + bottomHeight, box.left);
			layout += 1;
		}

		function frame(now: number) {
			raf = 0;
			if (quiet.matches || width === 0) return;
			const still = reduce.matches;
			const progress = still ? 1 : easeOut((now - started) / PRINT_MS);
			const y = window.scrollY;
			const view = window.innerHeight;
			const s = sizes(width);

			if (y < hostTop + topHeight) {
				const drift = still ? 0 : Math.min(y / DRIFT_PX, 1);
				const key = `${layout} ${colour} ${progress} ${drift}`;
				if (key !== drawnTop) {
					const fields = (scale: number): FritField[] => {
						const shrink = (1 - SHRINK * drift) * scale;
						const right = {
							x: width,
							y: 0,
							rx: s.right.rx * shrink,
							ry: s.right.ry * shrink,
							k: 1
						};
						if (!s.left) return [right];
						return [right, { x: 0, y: 0, rx: s.left.rx * shrink, ry: s.left.ry * shrink, k: 0.9 }];
					};
					paint(top!, topSmall!, fields, progress, topHeight, clearTop);
					drawnTop = key;
				}
			}

			// The bottom edge rises as it comes up into view.
			const floorInView = hostTop + floorTop - y;
			if (floorInView - bottomHeight < view) {
				const drift = still ? 0 : Math.min(Math.max((view - floorInView) / (view * 0.6), 0), 1);
				const key = `${layout} ${colour} ${progress} ${drift}`;
				if (key !== drawnBottom) {
					const fields = (scale: number): FritField[] => {
						const rx = s.bottom.rx * scale;
						const ry = s.bottom.ry * (1 + RISE * drift) * scale;
						return [
							{ x: 0, y: bottomHeight, rx, ry, k: 1 },
							{ x: width, y: bottomHeight, rx, ry, k: 1 }
						];
					};
					paint(bottom!, bottomSmall!, fields, progress, bottomHeight, clearBottom);
					drawnBottom = key;
				}
			}

			if (progress < 1) raf = requestAnimationFrame(frame);
			else breathe();
		}

		// Draws one band in full and, while it may breathe, at BREATH_LEAST
		// beneath it. The smaller fields cover only dots the full ones do, so
		// the full drawing alone shows while it is opaque.
		function paint(
			full: CanvasRenderingContext2D,
			least: CanvasRenderingContext2D,
			fields: (scale: number) => FritField[],
			progress: number,
			height: number,
			clear: FritClear[]
		) {
			const step = sizes(width).step;
			full.fillStyle = colour;
			drawFrit(full, fields(1), step, progress, width, height, clear);
			if (reduce.matches) {
				least.clearRect(0, 0, width, height);
				return;
			}
			least.fillStyle = colour;
			drawFrit(least, fields(BREATH_LEAST), step, progress, width, height, clear);
		}

		function breathe() {
			if (breaths.length > 0 || reduce.matches || quiet.matches) return;
			const keyframes = [{ opacity: 1 }, { opacity: 0 }];
			breaths = [topCanvas, bottomCanvas].map((canvas) =>
				canvas.animate(keyframes, {
					duration: BREATH_MS,
					easing: 'ease-in-out',
					direction: 'alternate',
					iterations: Infinity
				})
			);
			sync();
		}

		// Runs each breath only while the tab shows, its canvas is on screen
		// and the page is not scrolling; stops them for reduced motion.
		function sync() {
			if (reduce.matches || quiet.matches) {
				for (const breath of breaths) breath.cancel();
				breaths = [];
				return;
			}
			const run = !document.hidden && !scrolling;
			breaths.forEach((breath, i) => {
				const go = run && shown[i];
				if (go && breath.playState !== 'running') breath.play();
				else if (!go && breath.playState === 'running') breath.pause();
			});
		}

		function scrolled() {
			schedule();
			clearTimeout(scrolling);
			scrolling = window.setTimeout(() => {
				scrolling = 0;
				sync();
			}, SCROLL_REST_MS);
			sync();
		}

		function motionChanged() {
			relayout();
			sync();
		}

		function schedule() {
			if (!raf) raf = requestAnimationFrame(frame);
		}

		function relayout() {
			measure();
			schedule();
		}

		measure();
		schedule();

		const resized = new ResizeObserver(relayout);
		resized.observe(host);
		const themed = new MutationObserver(relayout);
		themed.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
		// Content that changes without resizing the page (a badge, a label)
		// moves what the dots keep clear of; wait for it to settle first.
		let settle = 0;
		const changed = new MutationObserver(() => {
			clearTimeout(settle);
			settle = window.setTimeout(relayout, 150);
		});
		changed.observe(host, {
			subtree: true,
			childList: true,
			characterData: true,
			attributes: true,
			attributeFilter: ['class', 'hidden']
		});
		// A canvas coming into view takes the other's place in the cycle, so
		// the two breathe together; it is at the screen's edge as it does.
		const onScreen = new IntersectionObserver((entries) => {
			for (const entry of entries) {
				const i = entry.target === topCanvas ? 0 : 1;
				shown[i] = entry.isIntersecting;
				const [mine, other] = [breaths[i], breaths[1 - i]];
				if (shown[i] && mine && other?.currentTime != null) mine.currentTime = other.currentTime;
			}
			sync();
		});
		onScreen.observe(topCanvas);
		onScreen.observe(bottomCanvas);
		window.addEventListener('scroll', scrolled, { passive: true });
		window.addEventListener('resize', relayout, { passive: true });
		document.addEventListener('visibilitychange', sync);
		quiet.addEventListener('change', motionChanged);
		reduce.addEventListener('change', motionChanged);

		return () => {
			cancelAnimationFrame(raf);
			clearTimeout(settle);
			clearTimeout(scrolling);
			for (const breath of breaths) breath.cancel();
			resized.disconnect();
			themed.disconnect();
			changed.disconnect();
			onScreen.disconnect();
			window.removeEventListener('scroll', scrolled);
			window.removeEventListener('resize', relayout);
			document.removeEventListener('visibilitychange', sync);
			quiet.removeEventListener('change', motionChanged);
			reduce.removeEventListener('change', motionChanged);
		};
	});
</script>

<div class="dn-decor dn-frit-frame" aria-hidden="true" bind:this={root}>
	<canvas bind:this={topLeast}></canvas>
	<canvas bind:this={bottomLeast}></canvas>
	<canvas bind:this={topCanvas}></canvas>
	<canvas bind:this={bottomCanvas}></canvas>
</div>

<style>
	.dn-frit-frame {
		position: absolute;
		inset: 0;
		z-index: -1;
		overflow: clip;
		pointer-events: none;
		color: var(--dn-frit);
		opacity: var(--dn-frit-opacity);
	}

	/* On paper the dots lean toward the acid, so they still read green-led. */
	:global([data-theme='light']) .dn-frit-frame {
		color: color-mix(in oklab, var(--dn-frit) 55%, var(--dn-action));
		opacity: 0.8;
	}

	canvas {
		position: absolute;
		top: 0;
		left: 0;
		display: block;
	}
</style>
