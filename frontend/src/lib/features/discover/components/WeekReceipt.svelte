<!--
	What the set was made from: the till strip on the counter's front rail,
	each figure a count the set stored when it was built, and "The receipt",
	which prints the whole sheet in a paper dialog.
-->
<script lang="ts">
	import { ChevronRight } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import Receipt, { type ReceiptSection } from '$lib/ui/Receipt.svelte';
	import ReceiptStrip, { type ReceiptCell } from '$lib/ui/ReceiptStrip.svelte';
	import type { WeeklySet } from '$lib/features/discover/queries/week.svelte';
	import {
		dayDate,
		dayMonth,
		figure,
		fullDate,
		shareOf
	} from '$lib/features/discover/state/weekWords';

	interface Props {
		set: WeeklySet | null;
		/** The receipt prints over the rail as the set arrives. */
		printing?: boolean;
	}

	let { set, printing = false }: Props = $props();

	let open = $state(false);

	const cells = $derived.by((): ReceiptCell[] => {
		if (!set) return [{ label: 'The receipt prints when the set is out' }];
		const { counts } = set;
		const picked: ReceiptCell = { figure: figure(set.picks.length), label: 'picked', total: true };
		if (!counts.linked) {
			return [
				{ figure: figure(counts.shelf_albums), label: 'albums on your shelves' },
				{ figure: figure(counts.shelf_artists), label: 'artists on your shelves' },
				{
					figure: figure(counts.plays_counted),
					label: counts.plays_counted === 0 ? 'plays linked yet' : 'plays in the app'
				},
				picked
			];
		}
		const list: ReceiptCell[] = [
			{ figure: figure(counts.plays_counted), label: 'plays, and your shelves' },
			{ figure: figure(counts.seeds), label: 'artists you play' }
		];
		if (counts.neighbours_found !== null && counts.neighbours_found !== undefined)
			list.push({ figure: figure(counts.neighbours_found), label: 'neighbours found' });
		if (counts.known_left_out !== null && counts.known_left_out !== undefined)
			list.push({
				figure: figure(counts.known_left_out),
				label: 'you know: left out',
				struck: true
			});
		list.push(picked);
		return list;
	});

	const sections = $derived.by((): ReceiptSection[] => {
		if (!set) return [];
		const { counts } = set;
		const range =
			counts.history_from_at && counts.history_to_at
				? `Your plays, ${dayMonth(counts.history_from_at)} to ${dayMonth(counts.history_to_at)}`
				: 'Your plays';
		const found = [];
		if (counts.neighbours_found !== null && counts.neighbours_found !== undefined)
			found.push({
				label: 'Neighbours of your artists',
				values: [figure(counts.neighbours_found)]
			});
		if (counts.known_left_out !== null && counts.known_left_out !== undefined)
			found.push({
				label: 'You already know: left out',
				values: [`-${figure(counts.known_left_out)}`]
			});
		if (counts.hubs_left_out !== null && counts.hubs_left_out !== undefined)
			found.push({ label: 'Liked by everybody: left out', values: ['yes'] });
		found.push({ label: 'Fame taken out of the ranking', values: ['yes'] });
		return [
			{
				head: 'Made from',
				rows: [
					{ label: range, values: [figure(counts.plays_counted)] },
					{ label: 'Artists you played twice or more', values: [figure(counts.seeds)] },
					{ label: 'Artists on your shelves', values: [figure(counts.shelf_artists)] }
				]
			},
			{ head: 'Found', rows: found },
			{
				head: 'Your tastes',
				columns: ['Share', 'Picks'],
				rows: set.tastes.map((taste) => ({
					label: taste.name,
					values: [shareOf(taste.share) ?? '', figure(taste.picks)]
				}))
			},
			{ head: 'Picked', value: figure(set.picks.length) },
			{ head: 'Next set', value: dayDate(set.next_set_at) }
		];
	});
</script>

<ReceiptStrip {cells} label="What this set was made from" print={printing} class="dn-receipt">
	{#snippet link()}
		{#if set}
			<Button
				variant="caps-link"
				size="sm"
				icon={ChevronRight}
				iconPosition="end"
				onclick={() => (open = true)}
			>
				The receipt
			</Button>
		{/if}
	{/snippet}
</ReceiptStrip>

{#if set}
	<Modal bind:open title="The receipt" paper bottomSheet closeButton size="sm">
		<Receipt
			title="Discover"
			subtitle="Week {set.week_number} - set out {fullDate(set.week_start_at)}"
			{sections}
		/>
	</Modal>
{/if}

<style>
	:global(.dn-receipt) {
		min-width: 0;
	}
</style>
