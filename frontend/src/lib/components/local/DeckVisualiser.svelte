<script lang="ts">
	import { playerStore } from '$lib/stores/player.svelte';
	import { tryGetAudioEngine } from '$lib/player/audioElement';

	interface Props {
		reducedMotion?: boolean;
	}

	let { reducedMotion = false }: Props = $props();

	let canvas = $state<HTMLCanvasElement>();
	// YouTube plays via a separate iframe and never produces spectrum data, so skip it (and idle) to avoid spinning the RAF loop for nothing.
	const reactive = $derived(
		playerStore.isPlaying &&
			!!playerStore.nowPlaying &&
			playerStore.nowPlaying.sourceType !== 'youtube'
	);

	const BARS = 56;
	const PEAK = 0.5;
	// Only the lower ~70% of bins carry useful energy; top bins sit near zero.
	const USABLE_FRACTION = 0.7;

	function cssVar(name: string, fallback: string): string {
		const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
		return v || fallback;
	}

	$effect(() => {
		const el = canvas;
		// Read deps so the effect re-runs (and re-cleans) on change.
		const active = reactive;
		const reduced = reducedMotion;
		if (!el) return;

		const ctx = el.getContext('2d');
		if (!ctx) return;

		const dpr = Math.min(window.devicePixelRatio || 1, 2);
		const resize = () => {
			el.width = Math.max(1, Math.floor(el.clientWidth * dpr));
			el.height = Math.max(1, Math.floor(el.clientHeight * dpr));
			ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		};
		resize();
		const ro = new ResizeObserver(resize);
		ro.observe(el);

		const clear = () => ctx.clearRect(0, 0, el.clientWidth, el.clientHeight);

		if (reduced || !active) {
			clear();
			return () => ro.disconnect();
		}

		const accent = cssVar('--color-accent', '#bbdb9b');
		const primary = cssVar('--color-primary', '#aed5f2');
		const heights = new Float32Array(BARS);
		let raf = 0;

		const draw = () => {
			raf = requestAnimationFrame(draw);
			const w = el.clientWidth;
			const h = el.clientHeight;
			ctx.clearRect(0, 0, w, h);

			const fft = tryGetAudioEngine()?.getFrequencyData() ?? null;
			const usable = fft ? Math.floor(fft.length * USABLE_FRACTION) : 0;

			const grad = ctx.createLinearGradient(0, h, 0, h * (1 - PEAK));
			grad.addColorStop(0, primary);
			grad.addColorStop(1, accent);
			ctx.fillStyle = grad;

			const slot = w / BARS;
			const barW = slot * 0.5;
			for (let i = 0; i < BARS; i++) {
				const src = usable ? Math.floor((i / BARS) * usable) : 0;
				// Gamma-curve the magnitude so quiet passages stay subtle.
				const target = fft ? (fft[src] / 255) ** 1.5 : 0;
				heights[i] += (target - heights[i]) * 0.2;
				const bh = heights[i] * h * PEAK;
				if (bh < 0.5) continue;
				const x = i * slot + (slot - barW) / 2;
				const r = Math.min(barW / 2, bh);
				ctx.beginPath();
				ctx.roundRect(x, h - bh, barW, bh, [r, r, 0, 0]);
				ctx.fill();
			}
		};
		raf = requestAnimationFrame(draw);

		return () => {
			cancelAnimationFrame(raf);
			ro.disconnect();
			clear();
		};
	});
</script>

<canvas
	bind:this={canvas}
	class="deck-visualiser pointer-events-none absolute inset-0 -z-10 h-full w-full"
	class:is-active={reactive && !reducedMotion}
	aria-hidden="true"
></canvas>

<style>
	.deck-visualiser {
		opacity: 0;
		filter: blur(2px);
		transition: opacity 0.9s ease;
		-webkit-mask-image: linear-gradient(to top, #000 0%, #000 42%, transparent 78%);
		mask-image: linear-gradient(to top, #000 0%, #000 42%, transparent 78%);
	}

	.deck-visualiser.is-active {
		opacity: 0.2;
	}

	@media (prefers-reduced-motion: reduce) {
		.deck-visualiser {
			opacity: 0 !important;
		}
	}
</style>
