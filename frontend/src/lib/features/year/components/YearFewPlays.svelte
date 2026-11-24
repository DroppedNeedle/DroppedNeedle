<!--
	A year with too few plays for a review: the year, its plays so far and
	one card saying when it fills in. The year switch stays, so earlier
	years can still be opened.
-->
<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import Card from '$lib/ui/Card.svelte';
	import HalftoneArt from '$lib/ui/HalftoneArt.svelte';
	import StatRow from '$lib/ui/StatRow.svelte';
	import { docsUrl } from '$lib/ui/docsUrl';
	import { outsideLink } from '$lib/ui/outsideLink';
	import YearSwitch from './YearSwitch.svelte';
	import { count } from './yearFormat';

	interface Props {
		review: YearReview;
		years: number[];
		/** Whether this is the year still running. */
		current: boolean;
	}

	let { review, years, current }: Props = $props();

	const stats = $derived([
		{ label: current ? 'Plays so far' : 'Plays', value: count(review.plays) }
	]);
</script>

<section class="yr-few" aria-label="Your year">
	<p class="yr-few__eyebrow">Your year</p>
	<h1 class="yr-few__year">{review.year}</h1>
	<StatRow {stats} label="Your year in numbers" class="yr-few__stats" />
	<Card variant="torn" class="yr-few__card">
		<HalftoneArt name="record" fade="left" class="yr-few__art" />
		<h2 class="yr-few__head">Not enough plays yet</h2>
		<p class="yr-few__body">
			Your year fills in from 30 plays and 5 artists. Play a few records here, or in an app you
			connect.
		</p>
		<a
			href={outsideLink(docsUrl('connect-your-app'))}
			target="_blank"
			rel="noopener noreferrer"
			class="yr-few__link"
		>
			How to connect an app<ArrowRight class="size-4" aria-hidden="true" />
		</a>
	</Card>
	<YearSwitch {years} year={review.year} />
</section>

<style>
	.yr-few {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 1.25rem;
		max-width: 36rem;
	}
	.yr-few__eyebrow {
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.yr-few__year {
		font-family: var(--font-display);
		font-weight: 400;
		font-size: clamp(6rem, 24vw, 10rem);
		line-height: 0.86;
		color: var(--dn-title);
		font-synthesis: none;
	}
	.yr-few :global(.yr-few__stats) {
		align-self: stretch;
		padding-block: 0.875rem;
		border-block: 1px solid var(--dn-line);
	}
	.yr-few :global(.yr-few__card) {
		position: relative;
		align-self: stretch;
		overflow: hidden;
		min-height: 13rem;
	}
	.yr-few :global(.yr-few__art) {
		position: absolute;
		right: -2.5rem;
		bottom: -2.5rem;
		width: 11rem;
		height: 11rem;
	}
	.yr-few__head {
		position: relative;
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 1.75rem;
		line-height: 1.05;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.yr-few__body {
		position: relative;
		max-width: 17rem;
		line-height: 1.45;
	}
	.yr-few__link {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 0.375rem;
		margin-top: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		text-decoration: underline;
		text-underline-offset: 0.3em;
	}
</style>
