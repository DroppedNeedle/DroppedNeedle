<script lang="ts">
	import type { ArtistIndexEntry, ArtistIndexArtist } from '$lib/types';
	import ArtistImage from '$lib/media/ArtistImage.svelte';

	type Props = {
		index: ArtistIndexEntry[];
		onselect?: (artist: ArtistIndexArtist) => void;
	};

	let { index, onselect }: Props = $props();

	let activeLetter = $state<string | null>(null);
	let container = $state<HTMLDivElement | null>(null);
	let sectionEls: Record<string, HTMLDivElement> = {};

	function scrollToLetter(letter: string) {
		activeLetter = letter;
		if (!container) return;
		const el = sectionEls[letter];
		if (!el) return;
		container.scrollTo({
			top: el.offsetTop - container.offsetTop,
			behavior: 'smooth'
		});
	}

	function handleContainerScroll() {
		if (!container) return;
		const scrollTop = container.scrollTop + container.offsetTop;
		let current: string | null = null;
		for (const entry of index) {
			if (entry.artists.length === 0) continue;
			const el = sectionEls[entry.name];
			if (el && el.offsetTop <= scrollTop + 20) {
				current = entry.name;
			}
		}
		if (current && current !== activeLetter) {
			activeLetter = current;
		}
	}

	const letters = $derived(index.map((e) => e.name).filter(Boolean));
</script>

<div class="flex gap-2">
	<nav
		class="flex flex-col items-center gap-0.5 sticky top-0 self-start pt-1"
		aria-label="Alphabetical index"
	>
		{#each letters as letter (letter)}
			<button
				class="text-xs font-bold px-1 py-0.5 rounded transition-colors hover:text-primary {activeLetter ===
				letter
					? 'text-primary'
					: 'text-base-content/50'}"
				onclick={() => scrollToLetter(letter)}
				aria-label="Jump to {letter}">{letter}</button
			>
		{/each}
	</nav>

	<div
		bind:this={container}
		class="flex-1 max-h-[60vh] overflow-y-auto rounded-lg pr-1"
		onscroll={handleContainerScroll}
	>
		{#each index as entry (entry.name)}
			{#if entry.artists.length > 0}
				<div bind:this={sectionEls[entry.name]} class="mb-3">
					<h3 class="text-sm font-bold text-primary sticky top-0 bg-base-100 py-1 z-10">
						{entry.name}
					</h3>
					<div class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-2">
						{#each entry.artists as artist (artist.id)}
							<button
								class="flex items-center gap-2 p-2 rounded-lg hover:bg-base-200 transition-colors cursor-pointer text-left"
								onclick={() => onselect?.(artist)}
							>
								<div class="w-8 h-8 rounded-full overflow-hidden shrink-0">
									<ArtistImage
										mbid={artist.musicbrainz_id ?? artist.id}
										remoteUrl={artist.image_url}
										alt={artist.name}
										size="full"
										requestSize={250}
									/>
								</div>
								<div class="min-w-0">
									<p class="text-sm font-medium truncate">{artist.name}</p>
									{#if artist.album_count != null}
										<p class="text-xs text-base-content/50">
											{artist.album_count} album{artist.album_count !== 1 ? 's' : ''}
										</p>
									{/if}
								</div>
							</button>
						{/each}
					</div>
				</div>
			{/if}
		{/each}
	</div>
</div>
