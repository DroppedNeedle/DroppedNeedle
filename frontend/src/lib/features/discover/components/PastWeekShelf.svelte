<!--
	One past week as a shelf: its tab (number, date and what was done with
	the week's picks) and its records, kept ones proud with their play
	count, passed ones low. Opening a record shows it as it was.
-->
<script lang="ts" module>
	import type { AnswerCounts } from '$lib/features/discover/queries/weeks.svelte';

	/** "1 kept - 1 later - 1 known - 5 open": the week's answers, none left out but zeros. */
	export function tallyWords(tally: AnswerCounts): string {
		return (
			[
				[tally.kept, 'kept'],
				[tally.later, 'later'],
				[tally.known, 'known'],
				[tally.passed, 'passed'],
				[tally.open, 'open']
			] as const
		)
			.filter(([n]) => n > 0)
			.map(([n, word]) => `${n} ${word}`)
			.join(' - ');
	}
</script>

<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import WeekShelf from '$lib/ui/WeekShelf.svelte';
	import type { PastPick as Pick, PastWeek } from '$lib/features/discover/queries/weeks.svelte';
	import { lookOf } from '$lib/features/discover/state/weekDeck.svelte';
	import { dayMonthYear } from '$lib/features/discover/state/weekWords';
	import PastPick from './PastPick.svelte';

	interface Props {
		week: PastWeek;
		/** Layout only: margin. */
		class?: string;
	}

	let { week, class: className = '' }: Props = $props();

	const items = $derived(
		week.picks.map((pick) => ({
			key: pick.album.musicbrainz_id ?? `pick-${pick.pick_no}`,
			title: pick.album.title,
			artist: pick.album.artist_name ?? undefined,
			state: lookOf(pick.state),
			plays: pick.plays ?? null,
			pick
		}))
	);

	let opened = $state<Pick | null>(null);
	let open = $state(false);
</script>

<WeekShelf
	{items}
	label="Week {week.week_number}"
	tab={{
		week: week.week_number,
		date: dayMonthYear(week.week_start_at),
		tally: tallyWords(week.tally)
	}}
	onopen={(item) => {
		opened = item.pick;
		open = true;
	}}
	class={className}
>
	{#snippet cover(item)}
		<AlbumImage
			mbid={item.pick.album.musicbrainz_id ?? ''}
			alt=""
			size="sm"
			rounded="none"
			className="size-full object-cover"
		/>
	{/snippet}
</WeekShelf>

<PastPick bind:open pick={opened} week={week.week_number} />
