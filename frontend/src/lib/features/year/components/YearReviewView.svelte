<!--
	One year of a person's own plays, as one long page: the year, top
	albums, top artists, top songs, new to you, by month, the receipt, and
	the other years at the foot. A list with too little in it is left out,
	and a year under the server's minimum shows only its count and a card.
	A year the list does not offer shows the not-found state.
-->
<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import {
		getYearReviewQuery,
		getYearReviewYearsQuery
	} from '$lib/features/year/queries/year.svelte';
	import BarcodeTicket from '$lib/ui/BarcodeTicket.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import YearFewPlays from './YearFewPlays.svelte';
	import YearHero from './YearHero.svelte';
	import YearMonths from './YearMonths.svelte';
	import YearNotFound from './YearNotFound.svelte';
	import YearNewArtists from './YearNewArtists.svelte';
	import YearReceipt from './YearReceipt.svelte';
	import YearTopAlbums from './YearTopAlbums.svelte';
	import YearTopArtists from './YearTopArtists.svelte';
	import YearTopSongs from './YearTopSongs.svelte';
	import { MIN_LIST, MIN_MONTHS, yearPath } from './yearFormat';

	interface Props {
		year: number;
	}

	let { year }: Props = $props();

	const yearsQuery = getYearReviewYearsQuery();
	const reviewQuery = getYearReviewQuery(() => year);

	const years = $derived(yearsQuery.data?.years.map((y) => y.year) ?? [year]);
	const current = $derived(yearsQuery.data?.years[0]?.year ?? new Date().getFullYear());
	const notFound = $derived(
		(yearsQuery.data !== undefined && !years.includes(year)) ||
			(reviewQuery.error instanceof ApiError && reviewQuery.error.status === 404)
	);
	const others = $derived(years.filter((y) => y !== year));
</script>

<svelte:head>
	{#if !notFound}<title>Your year {year} - DroppedNeedle</title>{/if}
</svelte:head>

<div class="yr-page">
	{#if notFound}
		<YearNotFound />
	{:else}
		<QueryBoundary query={reviewQuery} what="your year">
			{#snippet loading()}
				<div class="flex flex-col gap-6" aria-busy="true">
					<div class="skeleton h-40 w-72"></div>
					<div class="skeleton h-20 w-full max-w-2xl"></div>
					<div class="skeleton aspect-square w-full max-w-md"></div>
				</div>
			{/snippet}
			{#snippet children(review)}
				{#if review.state === 'too_few_plays'}
					<YearFewPlays {review} {years} current={year === current} />
				{:else}
					{@const months = review.months.filter((n) => n > 0).length}
					<YearHero {review} {years} />
					{#if review.top_albums.length >= MIN_LIST}
						<YearTopAlbums albums={review.top_albums} />
					{/if}
					{#if review.top_artists.length >= MIN_LIST}
						<YearTopArtists artists={review.top_artists} />
					{/if}
					{#if review.top_songs.length >= MIN_LIST}
						<YearTopSongs songs={review.top_songs} />
					{/if}
					{#if (review.new_artists?.count ?? 0) > 0 || months >= MIN_MONTHS}
						<div class="yr-page__pair">
							{#if review.new_artists && review.new_artists.count > 0}
								<YearNewArtists year={review.year} found={review.new_artists} />
							{/if}
							{#if months >= MIN_MONTHS}
								<YearMonths {review} />
							{/if}
						</div>
					{/if}
					<YearReceipt {review} />
					<footer class="yr-page__foot">
						{#if others.length > 0}
							<nav aria-label="Other years" class="yr-page__years">
								{#each others as other (other)}
									<a href={withBasePath(yearPath(other, current))} class="yr-page__year-link">
										{other}<ArrowRight class="size-4" aria-hidden="true" />
									</a>
								{/each}
							</nav>
						{/if}
						<BarcodeTicket label="DN / YR {review.year}" class="yr-page__ticket" />
					</footer>
				{/if}
			{/snippet}
		</QueryBoundary>
	{/if}
</div>

<style>
	.yr-page {
		display: flex;
		flex-direction: column;
		gap: 4.5rem;
		max-width: 80rem;
		padding: 2rem 1rem 4rem;
	}
	@media (min-width: 640px) {
		.yr-page {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.yr-page {
			padding: 3rem 2rem 5rem;
		}
	}
	@media (max-width: 639px) {
		.yr-page {
			gap: 3.5rem;
		}
	}

	.yr-page__pair {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 3.5rem;
	}
	@media (min-width: 1024px) {
		.yr-page__pair {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			gap: 4rem;
		}
	}

	.yr-page__foot {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 1.5rem;
		padding-top: 1.5rem;
		border-top: 1px solid var(--dn-line);
	}
	.yr-page__years {
		display: flex;
		flex-wrap: wrap;
		gap: 1.5rem;
	}
	.yr-page__year-link {
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-decoration: underline;
		text-underline-offset: 0.35em;
	}
	.yr-page__year-link:hover {
		color: var(--dn-title);
	}
	.yr-page__foot :global(.yr-page__ticket) {
		width: 9rem;
		margin-left: auto;
	}
</style>
