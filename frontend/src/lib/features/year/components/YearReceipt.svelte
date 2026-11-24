<!--
	The year's till receipt: plays and hours, the first play, new artists,
	the longest streak, the busiest day, days with music and the time zone
	it was counted in. Beside it, the index card on how it is counted.
-->
<script lang="ts">
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Receipt, { type ReceiptSection } from '$lib/ui/Receipt.svelte';
	import { count, hours, moment, weekday } from './yearFormat';

	interface Props {
		review: YearReview;
	}

	let { review }: Props = $props();

	const sections = $derived.by<ReceiptSection[]>(() => {
		const firsts = [];
		if (review.first_play) {
			firsts.push(
				{ label: 'First play', values: [moment(review.first_play.played_at_ms, review.zone)] },
				{ label: review.first_play.song, values: [] }
			);
		}
		if (review.new_artists) {
			firsts.push({ label: 'New artists', values: [count(review.new_artists.count)] });
		}
		const habits = [];
		if (review.longest_streak) {
			const days = review.longest_streak.days;
			habits.push({
				label: 'Longest streak',
				values: [`${count(days)} ${days === 1 ? 'day' : 'days'}`]
			});
		}
		if (review.busiest_day) {
			habits.push({
				label: 'Busiest day',
				values: [`${weekday(review.busiest_day.date)}, ${count(review.busiest_day.plays)}`]
			});
		}
		habits.push({ label: 'Days with music', values: [count(review.days_played)] });
		return [
			{
				head: 'Made from',
				rows: [
					{ label: 'Plays here', values: [count(review.plays)] },
					{ label: 'Hours', values: [count(hours(review.listened_ms))] }
				]
			},
			...(firsts.length > 0 ? [{ head: 'Firsts', rows: firsts }] : []),
			{ head: 'Habits', rows: habits },
			{ head: 'Plays', value: count(review.plays) },
			{ head: `Times in ${review.zone}` }
		];
	});
</script>

<div class="yr-till">
	<Receipt
		title={String(review.year)}
		subtitle="DroppedNeedle · Your year"
		{sections}
		class="yr-till__receipt"
	/>
	<Card variant="index" title="How it's counted" class="yr-till__card">
		<p>Plays made in DroppedNeedle and the apps you connect. Only you see this page.</p>
	</Card>
</div>

<style>
	.yr-till {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 2rem;
		align-items: start;
	}
	.yr-till :global(.yr-till__receipt) {
		width: 100%;
		max-width: 26rem;
		justify-self: center;
	}
	@media (min-width: 1024px) {
		.yr-till {
			grid-template-columns: minmax(0, 26rem) minmax(0, 22rem);
			justify-content: center;
			gap: 3rem;
		}
		.yr-till :global(.yr-till__card) {
			margin-top: 3rem;
		}
	}
</style>
