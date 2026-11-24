<!--
	The way into a person's year from Profile: this year, its plays and
	hours so far, and Open. It shows all year, under the review's minimum
	too; the counts wait for the read and are left out if it fails.
-->
<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';
	import { getYearReviewQuery } from '$lib/features/year/queries/year.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { count, hours } from './yearFormat';

	const year = new Date().getFullYear();
	const reviewQuery = getYearReviewQuery(() => year);
	const review = $derived(reviewQuery.data);
</script>

<Card
	variant="paper"
	padding="none"
	href={withBasePath('/your-year')}
	aria-label="Your year {year}"
>
	<div class="yr-card__row">
		<span class="yr-card__year">{year}</span>
		<span class="yr-card__words">
			<span class="yr-card__what">
				{review?.state === 'complete' ? 'Your year' : 'Your year so far'}
			</span>
			{#if review}
				<span class="yr-card__counts">
					{count(review.plays)}
					{review.plays === 1 ? 'play' : 'plays'}
					{#if review.state !== 'too_few_plays'}
						· {count(hours(review.listened_ms))}
						{hours(review.listened_ms) === 1 ? 'hour' : 'hours'}
					{/if}
				</span>
			{/if}
		</span>
		<span class="yr-card__open">Open <ArrowRight class="size-4" aria-hidden="true" /></span>
	</div>
</Card>

<style>
	.yr-card__row {
		display: flex;
		align-items: center;
		gap: 1.25rem;
		padding: 1.125rem 1.375rem;
	}
	.yr-card__year {
		font-family: var(--font-display);
		font-size: 3.5rem;
		line-height: 0.85;
		font-synthesis: none;
	}
	.yr-card__words {
		display: flex;
		flex: 1;
		flex-direction: column;
		gap: 0.25rem;
		min-width: 0;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	.yr-card__what {
		font-weight: 600;
		letter-spacing: 0.2em;
	}
	.yr-card__open {
		display: inline-flex;
		flex: none;
		align-items: center;
		gap: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	@media (max-width: 479px) {
		.yr-card__row {
			gap: 0.875rem;
			padding: 1rem;
		}
		.yr-card__year {
			font-size: 2.5rem;
		}
	}
</style>
