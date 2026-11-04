<!--
	One genre as a printed block: the Anton name, its plays as a mono line,
	and one bold abstract motif, picked from the name so a genre keeps its
	mark. The fill (paper or acid) comes from the grid, so neighbours differ.
	The motif is decoration and hidden from screen readers.
-->
<script lang="ts" module>
	export const GENRE_MOTIFS = [
		'bolt',
		'dots',
		'checks',
		'waves',
		'rays',
		'spikes',
		'bars',
		'grooves'
	] as const;
	export type GenreMotif = (typeof GENRE_MOTIFS)[number];

	/** A stable motif for a genre name. */
	export function genreMotif(name: string): GenreMotif {
		let hash = 0;
		for (let index = 0; index < name.length; index += 1) {
			hash = (hash * 31 + name.toLowerCase().charCodeAt(index)) | 0;
		}
		return GENRE_MOTIFS[Math.abs(hash) % GENRE_MOTIFS.length];
	}
</script>

<script lang="ts">
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		name: string;
		plays?: string | null;
		/** Layout only. */
		class?: string;
	}

	let { name, plays = null, class: className = '' }: Props = $props();

	const motif = $derived(genreMotif(name));

	// Each motif is drawn in a 100 x 64 box that fills the tile's right side
	// and runs past its right and bottom edges, where the tile clips it.
	const dots = Array.from({ length: 70 }, (_, i) => ({
		x: 5 + (i % 10) * 10,
		y: 4 + Math.floor(i / 10) * 10,
		r: 1.2 + (i % 10) * 0.42
	}));
	const checks = Array.from({ length: 35 }, (_, i) => ({
		x: (i % 7) * 16 - 6,
		y: Math.floor(i / 7) * 16 - 8,
		on: ((i % 7) + Math.floor(i / 7)) % 2 === 0
	})).filter((cell) => cell.on);
	const waves = [2, 13, 24, 35, 46, 57, 68];
	const rays = Array.from({ length: 13 }, (_, i) => {
		const at = (deg: number) => {
			const rad = (deg * Math.PI) / 180;
			return `${(104 + 150 * Math.cos(rad)).toFixed(1)} ${(68 + 150 * Math.sin(rad)).toFixed(1)}`;
		};
		const angle = 180 + i * 7.5;
		return `M104 68 L${at(angle - 2.4)} L${at(angle + 2.4)} Z`;
	});
	const bars = [22, 38, 30, 52, 44, 60, 34, 48, 26];
	const grooves = [14, 27, 40, 53, 66, 79, 92, 105, 118];
</script>

<a href={withBasePath(`/genre?name=${encodeURIComponent(name)}`)} class="genre-tile {className}">
	<span class="genre-tile__text">
		<span class="genre-tile__name">{name}</span>
		{#if plays}<span class="genre-tile__plays">{plays} plays</span>{/if}
	</span>
	<svg
		class="genre-tile__motif"
		viewBox="0 0 100 64"
		preserveAspectRatio="xMaxYMax slice"
		aria-hidden="true"
		focusable="false"
	>
		{#if motif === 'bolt'}
			<path d="M66 -8 L22 38 H50 L32 76 L94 20 H62 L86 -8 Z" fill="currentColor" />
		{:else if motif === 'dots'}
			{#each dots as dot (`${dot.x}-${dot.y}`)}
				<circle cx={dot.x} cy={dot.y} r={dot.r} fill="currentColor" />
			{/each}
		{:else if motif === 'checks'}
			{#each checks as cell (`${cell.x}-${cell.y}`)}
				<rect x={cell.x} y={cell.y} width="16" height="16" fill="currentColor" />
			{/each}
		{:else if motif === 'waves'}
			{#each waves as y (y)}
				<path
					d="M-10 {y} q12.5 -9 25 0 t25 0 t25 0 t25 0 t25 0"
					fill="none"
					stroke="currentColor"
					stroke-width="5"
				/>
			{/each}
		{:else if motif === 'rays'}
			{#each rays as ray (ray)}
				<path d={ray} fill="currentColor" />
			{/each}
			<circle cx="104" cy="68" r="16" fill="currentColor" />
		{:else if motif === 'spikes'}
			<path
				d="M-4 68 L6 30 L14 68 L24 6 L34 68 L44 22 L52 68 L62 -4 L72 68 L80 16 L90 68 L98 0 L106 68 Z"
				fill="currentColor"
			/>
		{:else if motif === 'bars'}
			{#each bars as h, i (i)}
				<rect x={2 + i * 11.5} y={66 - h} width="7.5" height={h} fill="currentColor" />
			{/each}
		{:else}
			{#each grooves as r (r)}
				<circle cx="104" cy="70" {r} fill="none" stroke="currentColor" stroke-width="5.5" />
			{/each}
		{/if}
	</svg>
</a>

<style>
	.genre-tile {
		position: relative;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		min-height: 4rem;
		overflow: hidden;
		border: 1px solid var(--tile-edge, var(--dn-paper-border));
		border-radius: 0.375rem;
		background: var(--tile-bg, var(--dn-paper));
		color: var(--tile-ink, var(--dn-paper-content));
		padding: 0.5rem 0.625rem 0.5rem 0.875rem;
		transition: translate 150ms;
	}
	@media (hover: hover) and (prefers-reduced-motion: no-preference) {
		.genre-tile:hover {
			translate: 0 -2px;
		}
	}
	.genre-tile:hover .genre-tile__name {
		text-decoration: underline;
		text-decoration-thickness: 2px;
		text-underline-offset: 3px;
	}

	.genre-tile__name {
		display: block;
		overflow: hidden;
		font-family: var(--font-display);
		font-size: 1.5rem;
		line-height: 1.05;
		letter-spacing: 0.01em;
		text-transform: uppercase;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-synthesis: none;
	}

	.genre-tile__plays {
		display: block;
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.04em;
		font-variant-numeric: tabular-nums;
	}

	/* the name keeps the left side; the motif never runs under it */
	.genre-tile__text {
		min-width: 0;
		max-width: 56%;
	}

	/* the motif fills the right side, full height, clipped by the tile */
	.genre-tile__motif {
		position: absolute;
		top: 0;
		right: 0;
		bottom: 0;
		width: 42%;
		height: 100%;
	}

	/* two to a row on phones: a smaller name, so a long one still fits */
	@media (max-width: 639px) {
		.genre-tile {
			padding-left: 0.625rem;
		}
		.genre-tile__name {
			font-size: clamp(1rem, 5vw, 1.2rem);
		}
	}

	@media (forced-colors: active) {
		.genre-tile__motif {
			display: none;
		}
	}
</style>
