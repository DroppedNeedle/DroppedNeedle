<!--
	A dark glass window with a faint grid that draws two sets of numbers over
	each other as traces: the `seed` in sound's plum with a soft fill, the
	`tuned` one in white. Both are scaled together from their own lowest and
	highest values, so their shapes and how far apart they run are kept, and
	each value is one step along, end to end; nothing is smoothed. It is one
	image named by `label`. The seed's trace draws itself in when it first
	shows and again only when `seedKey` changes (a new seed, not a new scale);
	the white one morphs when its numbers change, where the browser can. `pulse`, while given, nudges the white
	trace by a live level per point each frame (the player's analyser), never
	under reduced motion. The glass stays dark in every theme.
-->
<script lang="ts">
	import { prefersReducedMotion } from 'svelte/motion';

	interface Props {
		/** The sentence the picture is named by. */
		label: string;
		seed: readonly number[];
		/** What the seed is; the draw-in plays again only when it changes. */
		seedKey?: string | null;
		tuned?: readonly number[] | null;
		/** Read each frame while the white trace follows something playing: levels 0 to 255. */
		pulse?: (() => ArrayLike<number> | null) | null;
		/** Layout only: size, margin. */
		class?: string;
	}

	let {
		label,
		seed,
		seedKey = null,
		tuned = null,
		pulse = null,
		class: className = ''
	}: Props = $props();

	const W = 1000;
	const H = 300;
	/** Room left above and below the traces, as a share of the height. */
	const HEADROOM = 0.08;

	const range = $derived.by(() => {
		let low = Infinity;
		let high = -Infinity;
		for (const value of [...seed, ...(tuned ?? [])]) {
			if (!Number.isFinite(value)) continue;
			low = Math.min(low, value);
			high = Math.max(high, value);
		}
		if (low === Infinity) return { low: 0, span: 1 };
		return { low, span: Math.max(high - low, Number.EPSILON) };
	});

	function y(value: number): number {
		const share = (value - range.low) / range.span;
		return H - H * HEADROOM - share * H * (1 - 2 * HEADROOM);
	}

	function points(values: readonly number[], nudge?: (i: number) => number): string {
		const step = W / Math.max(values.length - 1, 1);
		return values
			.map((value, i) => {
				const at = Number.isFinite(value) ? y(value) : H / 2;
				const shift = nudge ? nudge(i) : 0;
				return `${i === 0 ? 'M' : 'L'}${(i * step).toFixed(1)} ${(at + shift).toFixed(1)}`;
			})
			.join(' ');
	}

	const seedLine = $derived(seed.length ? points(seed) : '');
	const seedFill = $derived(seedLine ? `${seedLine} L${W} ${H} L0 ${H} Z` : '');

	let level = $state<number[] | null>(null);
	const live = $derived(!!pulse && !!tuned?.length && !prefersReducedMotion.current);

	$effect(() => {
		const read = pulse;
		const count = tuned?.length ?? 0;
		if (!live || !read || !count) {
			level = null;
			return;
		}
		let raf = 0;
		const frame = () => {
			raf = requestAnimationFrame(frame);
			const bins = read();
			if (!bins || bins.length === 0) {
				level = null;
				return;
			}
			// The lower bins carry the music; spread them over the trace.
			const usable = Math.max(1, Math.floor(bins.length * 0.7));
			level = Array.from({ length: count }, (_, i) => bins[Math.floor((i / count) * usable)] / 255);
		};
		raf = requestAnimationFrame(frame);
		return () => cancelAnimationFrame(raf);
	});

	const tunedLine = $derived.by(() => {
		if (!tuned?.length) return '';
		const now = level;
		return now ? points(tuned, (i) => -(now[i] ?? 0) * H * 0.12) : points(tuned);
	});
</script>

<div class="dn-scope {className}" role="img" aria-label={label}>
	<svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" aria-hidden="true">
		{#if seedLine}
			{#key seedKey}
				<path class="dn-scope__fill" d={seedFill} />
				<path class="dn-scope__seed" d={seedLine} pathLength="1" data-trace="seed" />
			{/key}
		{/if}
		{#if tunedLine}
			<path
				class="dn-scope__tuned"
				class:is-live={!!level}
				d={tunedLine}
				style:d={`path('${tunedLine}')`}
				data-trace="tuned"
			/>
		{/if}
	</svg>
</div>

<style>
	.dn-scope {
		position: relative;
		overflow: hidden;
		border-radius: 0.75rem;
		background-color: var(--dn-vinyl);
		background-image:
			linear-gradient(to right, rgb(255 255 255 / 0.05) 1px, transparent 1px),
			linear-gradient(to bottom, rgb(255 255 255 / 0.05) 1px, transparent 1px);
		background-size: 10% 25%;
		box-shadow:
			inset 0 2px 8px rgb(0 0 0 / 0.7),
			inset 0 0 0 1px rgb(0 0 0 / 0.6),
			0 1px 0 rgb(255 255 255 / 0.06);
	}
	.dn-scope svg {
		display: block;
		width: 100%;
		height: 100%;
		overflow: visible;
	}
	.dn-scope path {
		vector-effect: non-scaling-stroke;
		stroke-linejoin: round;
		stroke-linecap: round;
	}
	.dn-scope__fill {
		fill: color-mix(in oklab, var(--dn-sound) 14%, transparent);
		stroke: none;
		animation: dn-scope-fill 1.1s ease-out both;
	}
	.dn-scope__seed {
		fill: none;
		stroke: var(--dn-sound);
		stroke-width: 2;
		stroke-dasharray: 1;
		animation: dn-scope-draw 1.1s ease-out both;
	}
	.dn-scope__tuned {
		fill: none;
		stroke: var(--dn-arm);
		stroke-width: 1.5;
		transition: d 0.38s ease;
	}
	.dn-scope__tuned.is-live {
		transition: none;
	}

	@keyframes dn-scope-draw {
		from {
			stroke-dashoffset: 1;
		}
		to {
			stroke-dashoffset: 0;
		}
	}
	@keyframes dn-scope-fill {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	@media (forced-colors: active) {
		.dn-scope__seed {
			stroke: Highlight;
		}
		.dn-scope__tuned {
			stroke: CanvasText;
		}
	}
</style>
