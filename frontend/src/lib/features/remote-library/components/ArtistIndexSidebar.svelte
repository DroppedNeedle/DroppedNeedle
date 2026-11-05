<script lang="ts">
	import type {
		RemoteArtist,
		RemoteArtistIndexEntry
	} from '$lib/features/remote-library/queries/types';
	import ArtistImage from '$lib/media/ArtistImage.svelte';

	type Props = {
		index: RemoteArtistIndexEntry[];
		onselect?: (artist: RemoteArtist) => void;
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

<div class="az">
	<nav class="az__rail" aria-label="Alphabetical index">
		{#each letters as letter (letter)}
			<button
				type="button"
				class="az__letter"
				class:az__letter--on={activeLetter === letter}
				onclick={() => scrollToLetter(letter)}
				aria-label="Jump to {letter}">{letter}</button
			>
		{/each}
	</nav>

	<div bind:this={container} class="az__list" onscroll={handleContainerScroll}>
		{#each index as entry (entry.name)}
			{#if entry.artists.length > 0}
				<div bind:this={sectionEls[entry.name]} class="az__group">
					<h3 class="az__head">{entry.name}</h3>
					<div class="az__grid">
						{#each entry.artists as artist (artist.id)}
							<button type="button" class="az__artist" onclick={() => onselect?.(artist)}>
								<span class="az__pic">
									<ArtistImage
										mbid={artist.artist_mbid ?? artist.id}
										remoteUrl={artist.image_url}
										alt={artist.name}
										size="full"
										requestSize={250}
									/>
								</span>
								<span class="min-w-0">
									<span class="block truncate text-sm font-semibold">{artist.name}</span>
									{#if artist.album_count != null}
										<span class="az__meta">
											{artist.album_count} album{artist.album_count !== 1 ? 's' : ''}
										</span>
									{/if}
								</span>
							</button>
						{/each}
					</div>
				</div>
			{/if}
		{/each}
	</div>
</div>

<style>
	.az {
		display: flex;
		gap: 0.75rem;
	}
	.az__rail {
		position: sticky;
		top: 0;
		display: flex;
		flex: none;
		flex-direction: column;
		align-self: flex-start;
		gap: 0.0625rem;
		padding: 0.25rem;
		border-right: 1px solid var(--dn-line);
	}
	.az__letter {
		width: 1.75rem;
		padding-block: 0.0625rem;
		border-radius: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		color: var(--dn-muted);
		transition: color 150ms;
	}
	.az__letter:hover {
		color: var(--dn-title);
	}
	.az__letter--on {
		background: var(--dn-action);
		color: var(--dn-action-content);
	}
	.az__letter--on:hover {
		color: var(--dn-action-content);
	}
	.az__letter:focus-visible,
	.az__artist:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: 1px;
	}

	.az__list {
		max-height: 60vh;
		min-width: 0;
		flex: 1;
		overflow-y: auto;
		padding-right: 0.25rem;
	}
	.az__group {
		margin-bottom: 1rem;
	}
	.az__head {
		position: sticky;
		top: 0;
		z-index: 1;
		padding-block: 0.125rem 0.375rem;
		background: var(--color-base-200);
		font-family: var(--font-display);
		font-size: 1.75rem;
		font-weight: 400;
		line-height: 1;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.az__grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 13rem), 1fr));
		gap: 0.25rem 0.75rem;
	}
	.az__artist {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.375rem;
		border-radius: 0.375rem;
		text-align: left;
		transition: background-color 150ms;
	}
	.az__artist:hover {
		background: color-mix(in oklab, var(--dn-title) 7%, transparent);
	}
	.az__pic {
		width: 2.5rem;
		height: 2.5rem;
		flex: none;
		overflow: hidden;
		border-radius: 999px;
	}
	.az__meta {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.06em;
		color: var(--dn-muted);
	}
</style>
