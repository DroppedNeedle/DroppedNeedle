<script lang="ts">
	import { resolve } from '$app/paths';
	import { Globe, Shuffle } from 'lucide-svelte';

	interface Props {
		title: string;
		genres: { name: string; listen_count?: number | null; artist_count?: number | null }[];
		onShuffle?: () => void;
	}

	let { title, genres, onShuffle }: Props = $props();

	const MAX_PILLS = 15;

	const genreColors = [
		'from-rose-500/90 to-pink-700',
		'from-violet-500/90 to-purple-700',
		'from-blue-500/90 to-cyan-700',
		'from-emerald-500/90 to-teal-700',
		'from-amber-500/90 to-orange-700',
		'from-red-500/90 to-rose-700',
		'from-indigo-500/90 to-violet-700',
		'from-cyan-500/90 to-blue-700',
		'from-green-500/90 to-emerald-700',
		'from-orange-500/90 to-amber-700'
	];

	function getGenreColor(name: string): string {
		let hash = 0;
		for (let i = 0; i < name.length; i++) {
			hash = (hash * 31 + name.charCodeAt(i)) | 0;
		}
		return genreColors[Math.abs(hash) % genreColors.length];
	}

	function formatCount(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
		return n.toString();
	}
</script>

<section>
	<div class="mb-5 flex items-center gap-2">
		<Globe class="size-5 text-primary" />
		<h2 class="text-lg font-bold sm:text-xl">{title}</h2>
		{#if onShuffle}
			<button
				class="btn btn-ghost btn-sm btn-circle ml-auto tooltip tooltip-left"
				data-tip="Shuffle genres"
				onclick={onShuffle}
				aria-label="Shuffle genres"
			>
				<Shuffle class="size-4" />
			</button>
		{/if}
	</div>

	<div class="flex flex-wrap gap-3">
		{#each genres.slice(0, MAX_PILLS) as genre (genre.name)}
			<a
				href={resolve(`/genre?name=${encodeURIComponent(genre.name)}`)}
				class="genre-pill group relative inline-flex items-center gap-1.5 rounded-full bg-gradient-to-r px-4 py-2 text-sm font-medium text-white shadow-lg ring-1 ring-inset ring-white/15 transition-all duration-300 motion-safe:hover:scale-105 hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 sm:px-5 sm:py-2.5 {getGenreColor(
					genre.name
				)}"
				aria-label="{genre.name}{genre.listen_count
					? ` - ${formatCount(genre.listen_count)} listens`
					: ''}"
			>
				<span>{genre.name}</span>
				{#if genre.listen_count}
					<span
						class="ml-0.5 rounded-full bg-black/30 px-1.5 py-0.5 text-[11px] leading-none text-white/90"
						aria-hidden="true"
					>
						{formatCount(genre.listen_count)}
					</span>
				{/if}
			</a>
		{/each}
	</div>
</section>

<style>
	.genre-pill {
		box-shadow:
			0 4px 14px rgba(0, 0, 0, 0.3),
			0 1px 3px rgba(0, 0, 0, 0.2),
			inset 0 1px 0 rgba(255, 255, 255, 0.1);
	}

	.genre-pill:hover {
		transform: translateY(-2px) scale(1.05);
		box-shadow:
			0 8px 24px rgba(0, 0, 0, 0.4),
			0 4px 8px rgba(0, 0, 0, 0.25),
			inset 0 1px 0 rgba(255, 255, 255, 0.15);
	}

	@media (prefers-reduced-motion: reduce) {
		.genre-pill {
			animation: none;
		}
		.genre-pill:hover {
			transform: scale(1.05);
		}
	}
</style>
