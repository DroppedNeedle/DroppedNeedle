<script lang="ts">
	import { resolve } from '$app/paths';
	import GenreArtwork from '$lib/components/GenreArtwork.svelte';
	import type { GenreArtwork as GenreArtworkModel } from '$lib/types';
	import { getGenreGradient } from '$lib/utils/genreGradient';

	interface Props {
		title: string;
		genres: { name: string; listen_count?: number | null; artist_count?: number | null }[];
		genreArtwork?: Record<string, GenreArtworkModel> | undefined;
	}

	let { title, genres, genreArtwork = undefined }: Props = $props();

	function formatCount(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
		return n.toString();
	}
</script>

<section>
	<div class="mb-4 flex items-center justify-between">
		<h2 class="text-lg font-bold sm:text-xl">{title}</h2>
	</div>
	<div class="grid grid-cols-2 gap-2.5 sm:grid-cols-3 sm:gap-3 md:grid-cols-4 lg:grid-cols-5">
		{#each genres.slice(0, 20) as genre (genre.name)}
			<a
				href={resolve(`/genre?name=${encodeURIComponent(genre.name)}`)}
				class="genre-tile group relative isolate overflow-hidden rounded-xl text-white shadow-md transition-all duration-300 hover:shadow-xl hover:ring-2 hover:ring-white/20 active:scale-[0.97] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary"
			>
				<div class="aspect-16/10"></div>

				<div class="pointer-events-none absolute inset-0" style="z-index: 1;">
					<GenreArtwork
						artwork={genreArtwork?.[genre.name]}
						gradientClass={getGenreGradient(genre.name)}
					/>
				</div>

				<div
					class="absolute inset-0 bg-linear-to-t from-black/70 via-black/20 to-transparent"
					style="z-index: 6;"
				></div>

				<div
					class="absolute inset-x-0 bottom-0 flex flex-col justify-end p-3 sm:p-4"
					style="z-index: 10;"
				>
					{#if genre.listen_count}
						<span class="mb-1 text-[10px] font-medium tracking-wide text-white/70 sm:text-xs">
							{formatCount(genre.listen_count)} plays
						</span>
					{/if}
					<h3 class="line-clamp-2 text-sm font-bold leading-tight drop-shadow-md sm:text-base">
						{genre.name}
					</h3>
				</div>

				<div
					class="pointer-events-none absolute inset-0 rounded-xl ring-1 ring-inset ring-white/10 transition-all duration-300 group-hover:ring-white/25"
					style="z-index: 15;"
				></div>
			</a>
		{/each}
	</div>
</section>
