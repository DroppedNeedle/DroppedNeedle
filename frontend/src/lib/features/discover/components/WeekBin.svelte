<!--
	This week's picks in a bin, filed behind a divider tab for each of the
	person's tastes, with what became of each and whether they heard it. The
	pick a sample is playing from shows three moving bars.
	The pick on the counter is pulled forward as a paper slip. While the set
	is being put out the bin shows its eight places, filled as they are
	found.
-->
<script lang="ts">
	import { Headphones } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Bin, { type BinGroup, type BinRow } from '$lib/ui/Bin.svelte';
	import StatusChip, { type StateLook } from '$lib/ui/StatusChip.svelte';
	import type { WeeklySet } from '$lib/features/discover/queries/week.svelte';
	import { countWord, shareOf } from '$lib/features/discover/state/weekWords';

	interface Row extends BinRow {
		mbid: string;
		look?: StateLook;
		heard: boolean;
		slot?: boolean;
	}

	interface Props {
		set: WeeklySet | null;
		/** What each pick reads as now, by its key. */
		looks: Map<string, StateLook | undefined>;
		/** The key of the pick on the counter; none once every pick is answered. */
		current: string | null;
		/** The key of the pick a sample is playing from. */
		playing?: string | null;
		answered: number;
		label: string;
		/** While the set is put out: how many of eight are found. */
		found?: number | null;
		arriving?: boolean;
		onselect: (key: string) => void;
	}

	let {
		set,
		looks,
		current,
		playing = null,
		answered,
		label,
		found = null,
		arriving = false,
		onselect
	}: Props = $props();

	const SET_SIZE = 8;
	const pad = (n: number) => String(n).padStart(2, '0');

	const groups = $derived.by((): BinGroup<Row>[] => {
		if (!set) {
			return [
				{
					rows: Array.from({ length: SET_SIZE }, (_, i) => ({
						key: `slot-${i}`,
						mbid: '',
						number: pad(i + 1),
						title: found !== null && i < found ? 'Found' : found === i ? 'Looking' : '',
						artist: '',
						heard: false,
						slot: true
					}))
				}
			];
		}
		const rowOf = (pick: WeeklySet['picks'][number]): Row => {
			const key = pick.album.musicbrainz_id ?? `pick-${pick.pick_no}`;
			const look = looks.get(key);
			return {
				key,
				mbid: pick.album.musicbrainz_id ?? '',
				number: pad(pick.pick_no),
				title: pick.album.title,
				artist: pick.album.artist_name ?? '',
				struck: look === 'passed',
				look,
				heard: pick.heard
			};
		};
		const filed: BinGroup<Row>[] = set.tastes.map((taste, i) => {
			const share = shareOf(taste.share);
			return {
				name: taste.name,
				share: share ?? undefined,
				shareSpoken: share ? `${share} of your listening` : undefined,
				rows: set.picks.filter((pick) => pick.taste_no === i).map(rowOf)
			};
		});
		const further = set.picks.filter(
			(pick) => pick.taste_no === null || pick.taste_no === undefined || !set.tastes[pick.taste_no]
		);
		if (further.length) filed.push({ rows: further.map(rowOf) });
		return filed.filter((group) => group.rows.length > 0);
	});

	const title = $derived(`This week's ${countWord(set?.picks.length ?? SET_SIZE)}`);
	const tally = $derived(
		set ? `${answered} of ${set.picks.length} answered` : `${found ?? 0} of ${SET_SIZE} found`
	);
</script>

<Bin
	{title}
	{tally}
	{label}
	{groups}
	current={current ?? undefined}
	{arriving}
	onselect={set ? (row) => onselect(row.key) : undefined}
	class="dn-week-bin"
>
	{#snippet cover(row)}
		{#if row.slot}
			<span class="dn-week-bin__slot" aria-hidden="true"></span>
		{:else}
			<AlbumImage
				mbid={row.mbid}
				alt=""
				size="xs"
				rounded="none"
				className="size-full object-cover"
			/>
		{/if}
	{/snippet}
	{#snippet state(row, isCurrent)}
		{#if row.key === playing}
			<span class="dn-week-bin__bars" aria-hidden="true"><i></i><i></i><i></i></span>
			<span class="sr-only">Playing</span>
		{/if}
		{#if isCurrent}
			<span class="dn-week-bin__here">On the counter</span>
		{:else}
			{#if row.heard}
				<span class="dn-week-bin__heard">
					<Headphones class="size-3.5" aria-hidden="true" />
					{#if row.look}<span class="sr-only">Heard</span>{:else}Heard{/if}
				</span>
			{/if}
			{#if row.look}<StatusChip look={row.look} />{/if}
		{/if}
	{/snippet}
	{#snippet foot()}
		{#if set?.short_line}
			<p class="dn-week-bin__short">
				<b>Only {set.picks.length} this week.</b>
				{set.short_line}
			</p>
		{/if}
	{/snippet}
</Bin>

<style>
	.dn-week-bin__slot {
		display: block;
		width: 100%;
		height: 100%;
		border: 1.5px dashed var(--dn-line);
	}
	.dn-week-bin__here {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
	}
	.dn-week-bin__heard {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-week-bin__bars {
		display: inline-flex;
		align-items: flex-end;
		gap: 2px;
		height: 14px;
		margin-right: 0.375rem;
	}
	.dn-week-bin__bars i {
		width: 3px;
		height: 8px;
		border-radius: 1px;
		background: currentColor;
		animation: dn-week-bin-bar 0.8s ease-in-out infinite;
	}
	.dn-week-bin__bars i:nth-child(2) {
		height: 14px;
		animation-duration: 0.6s;
		animation-delay: -0.2s;
	}
	.dn-week-bin__bars i:nth-child(3) {
		height: 5px;
		animation-duration: 0.7s;
		animation-delay: -0.4s;
	}
	@keyframes dn-week-bin-bar {
		0%,
		100% {
			height: 4px;
		}
		50% {
			height: 14px;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.dn-week-bin__bars i {
			animation: none;
		}
	}
	.dn-week-bin__short {
		margin: 0.75rem 0.375rem;
		font-size: 0.8125rem;
		line-height: 1.45;
		color: var(--dn-muted);
	}
	.dn-week-bin__short b {
		display: block;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
</style>
