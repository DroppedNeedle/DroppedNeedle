<!--
	The artists first played this year: how many, up to eight of them by
	plays, and the first one met.
-->
<script lang="ts">
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import { artistHref } from '$lib/utils/entityRoutes';
	import { count, day, plays } from './yearFormat';

	interface Props {
		year: number;
		found: NonNullable<YearReview['new_artists']>;
	}

	let { year, found }: Props = $props();
</script>

<section aria-label="New to you" class="yr-new">
	<SectionHead title="New to you" />
	<div class="yr-new__count">
		<span class="yr-new__figure">{count(found.count)}</span>
		<span class="yr-new__lines">
			<span>{found.count === 1 ? 'Artist' : 'Artists'} first played in {year}</span>
			{#if found.first}
				<span>First: {found.first.artist.name}, {day(found.first.date)}</span>
			{/if}
		</span>
	</div>
	{#if found.artists.length > 0}
		<ul class="yr-new__list">
			{#each found.artists.slice(0, 8) as artist (artist.id ?? artist.name)}
				<li class="yr-new__row">
					<span class="yr-new__face">
						<ArtistImage
							mbid={artist.mbid ?? ''}
							localId={artist.id}
							artMissing={artist.art_missing || (!artist.id && !artist.mbid)}
							alt={artist.name}
							size="full"
							requestSize={250}
							className="h-full w-full"
						/>
					</span>
					<span class="yr-new__words">
						{#if artist.id}
							<a href={artistHref(artist.id)} class="yr-new__name">{artist.name}</a>
						{:else}
							<span class="yr-new__name">{artist.name}</span>
						{/if}
						<span class="yr-new__plays">{plays(artist.plays)}</span>
					</span>
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.yr-new {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		min-width: 0;
	}
	.yr-new__count {
		display: flex;
		align-items: center;
		gap: 1.25rem;
	}
	.yr-new__figure {
		font-family: var(--font-display);
		font-size: 6rem;
		line-height: 0.9;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.yr-new__lines {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}

	.yr-new__list {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.875rem 1rem;
		list-style: none;
	}
	.yr-new__row {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		min-width: 0;
	}
	.yr-new__face {
		position: relative;
		flex: none;
		width: 3rem;
		height: 3rem;
		overflow: hidden;
		border-radius: 9999px;
	}
	.yr-new__words {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}
	.yr-new__name {
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	a.yr-new__name:hover {
		color: var(--dn-title);
	}
	.yr-new__plays {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.12em;
		color: var(--dn-muted);
	}

	@media (max-width: 639px) {
		.yr-new__figure {
			font-size: 4.5rem;
		}
		.yr-new__lines {
			font-size: 0.6875rem;
		}
	}
</style>
