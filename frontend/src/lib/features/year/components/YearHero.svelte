<!--
	The top of a year's page: the year in the display face, "SO FAR" while
	it is open, the days it covers, its counts, the year switch, and the
	No. 1 album on its sleeve.
-->
<script lang="ts">
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import StatRow, { type Stat } from '$lib/ui/StatRow.svelte';
	import Tape from '$lib/ui/Tape.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import YearSwitch from './YearSwitch.svelte';
	import { count, day, hours, plays } from './yearFormat';

	interface Props {
		review: YearReview;
		years: number[];
	}

	let { review, years }: Props = $props();

	const stats = $derived<Stat[]>([
		{ label: 'Hours', value: count(hours(review.listened_ms)) },
		{ label: 'Plays', value: count(review.plays) },
		{ label: 'Artists', value: count(review.artists) },
		...(review.new_artists ? [{ label: 'New to you', value: count(review.new_artists.count) }] : [])
	]);
	const top = $derived(review.top_albums[0] ?? null);
</script>

<section class="yr-hero" aria-label="Your year">
	<div class="yr-hero__text">
		<p class="yr-hero__eyebrow">Your year</p>
		<div class="yr-hero__line">
			<h1 class="yr-hero__year">{review.year}</h1>
			{#if review.state === 'in_progress'}<Tape look="ink" class="yr-hero__tape">So far</Tape>{/if}
		</div>
		<p class="yr-hero__span">
			{day(review.first_day)} - {day(review.last_day)} · Plays made here
		</p>
		<StatRow {stats} label="Your year in numbers" class="yr-hero__stats" />
		<div class="yr-hero__actions">
			<YearSwitch {years} year={review.year} />
		</div>
	</div>

	{#if top}
		<figure class="yr-hero__top">
			<div class="yr-hero__sleeve">
				<VinylSleeve class="w-full">
					<a href={albumHref(top.id)} class="block aspect-square" aria-label={top.name}>
						<AlbumImage
							mbid={top.release_group_mbid ?? ''}
							albumId={top.id}
							available={top.cover_available}
							artMissing={top.art_missing}
							alt={top.name}
							size="full"
							requestSize={500}
							rounded="none"
							priority
							lazy={false}
							className="h-full w-full"
						/>
					</a>
				</VinylSleeve>
				<Tape class="yr-hero__label">No. 1 album</Tape>
			</div>
			<figcaption class="yr-hero__caption">
				<span class="yr-hero__album">{top.name}</span>
				<span class="yr-hero__meta">{top.artist_name} · {plays(top.plays)}</span>
			</figcaption>
		</figure>
	{/if}
</section>

<style>
	.yr-hero {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 2rem;
		align-items: center;
	}
	@media (min-width: 1024px) {
		.yr-hero {
			grid-template-columns: minmax(0, 1fr) minmax(0, 30rem);
			gap: 4rem;
		}
	}

	.yr-hero__text {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		min-width: 0;
	}

	.yr-hero__eyebrow,
	.yr-hero__span {
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
	}
	.yr-hero__eyebrow {
		color: var(--dn-muted);
	}

	.yr-hero__line {
		display: flex;
		align-items: flex-start;
		gap: 1.25rem;
	}
	.yr-hero__year {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: clamp(6rem, 24vw, 12.5rem);
		line-height: 0.86;
		letter-spacing: 0.01em;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.yr-hero__line :global(.yr-hero__tape) {
		margin-top: 1rem;
	}

	.yr-hero :global(.yr-hero__stats) {
		padding-block: 1rem;
		border-block: 1px solid var(--dn-line);
	}

	.yr-hero__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 1rem;
	}
	.yr-hero__actions:empty {
		display: none;
	}

	.yr-hero__top {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		min-width: 0;
	}
	.yr-hero__sleeve {
		position: relative;
	}
	.yr-hero__sleeve :global(.yr-hero__label) {
		position: absolute;
		top: -0.75rem;
		left: -0.5rem;
		z-index: 2;
	}

	.yr-hero__caption {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		min-width: 0;
	}
	.yr-hero__album {
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1.05;
		text-transform: uppercase;
		overflow-wrap: anywhere;
	}
	.yr-hero__meta {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	@media (max-width: 639px) {
		.yr-hero__eyebrow,
		.yr-hero__span {
			font-size: 0.6875rem;
			letter-spacing: 0.16em;
		}
	}
</style>
