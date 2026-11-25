<!--
	The year by month: twelve bars, the busiest filled in the action colour
	and the months still to come drawn as outlines, then the album on
	repeat in the busiest month. The bars are a picture; a table under them
	gives a screen reader the same numbers.
-->
<script lang="ts">
	import type { YearReview } from '$lib/features/year/queries/year.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { count, monthName, monthOf } from './yearFormat';

	interface Props {
		review: YearReview;
	}

	let { review }: Props = $props();

	const lastMonth = $derived(
		review.last_day.startsWith(`${review.year}-`) ? (monthOf(review.last_day) ?? 12) : 12
	);
	const most = $derived(Math.max(1, ...review.months));
	const busiest = $derived(review.busiest_month);
	const bars = $derived(
		review.months.slice(0, 12).map((n, i) => ({
			month: i + 1,
			plays: n,
			height: `${Math.max(n > 0 ? 3 : 0, Math.round((n / most) * 100))}%`,
			busiest: busiest?.month === i + 1,
			ahead: i + 1 > lastMonth
		}))
	);
</script>

<section aria-label="By month" class="yr-months">
	<SectionHead title="By month" />
	<div class="yr-months__chart" aria-hidden="true">
		{#each bars as bar (bar.month)}
			<div class="yr-months__col">
				<div class="yr-months__track">
					<div
						class="yr-months__bar"
						class:is-busiest={bar.busiest}
						class:is-ahead={bar.ahead}
						style:height={bar.ahead ? '100%' : bar.height}
					></div>
				</div>
				<span class="yr-months__label">{monthName(bar.month).slice(0, 1)}</span>
			</div>
		{/each}
	</div>
	<table class="sr-only">
		<caption>Plays by month</caption>
		<thead>
			<tr><th scope="col">Month</th><th scope="col">Plays</th></tr>
		</thead>
		<tbody>
			{#each bars as bar (bar.month)}
				{#if !bar.ahead}
					<tr><th scope="row">{monthName(bar.month)}</th><td>{count(bar.plays)}</td></tr>
				{/if}
			{/each}
		</tbody>
	</table>

	{#if busiest?.album}
		{@const album = busiest.album}
		<a href={albumHref(album.id)} class="yr-months__repeat">
			<span class="yr-months__cover">
				<AlbumImage
					mbid={album.release_group_mbid ?? ''}
					albumId={album.id}
					available={album.cover_available}
					artMissing={album.art_missing}
					alt={album.name}
					size="full"
					requestSize={250}
					rounded="sm"
					className="h-full w-full"
				/>
			</span>
			<span class="yr-months__words">
				<span class="yr-months__when">{monthName(busiest.month)} · On repeat</span>
				<span class="yr-months__album">
					{album.name} · {count(album.plays)}
					{album.plays === 1 ? 'play' : 'plays'}
				</span>
			</span>
		</a>
	{/if}
</section>

<style>
	.yr-months {
		display: flex;
		flex-direction: column;
		gap: 1.25rem;
		min-width: 0;
	}

	.yr-months__chart {
		display: grid;
		grid-template-columns: repeat(12, minmax(0, 1fr));
		gap: 0.375rem;
		height: 11rem;
	}
	.yr-months__col {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		min-width: 0;
	}
	.yr-months__track {
		display: flex;
		flex: 1;
		align-items: flex-end;
	}
	.yr-months__bar {
		width: 100%;
		border-radius: 2px 2px 0 0;
		background-color: var(--dn-line);
	}
	.yr-months__bar.is-busiest {
		background-color: var(--dn-action);
	}
	.yr-months__bar.is-ahead {
		border: 1px dashed var(--dn-edge);
		background-color: transparent;
	}
	.yr-months__label {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		text-align: center;
		color: var(--dn-muted);
	}

	.yr-months__repeat {
		display: flex;
		align-items: center;
		gap: 0.875rem;
		min-width: 0;
		border-radius: 0.25rem;
	}
	.yr-months__cover {
		position: relative;
		flex: none;
		width: 3.5rem;
		height: 3.5rem;
	}
	.yr-months__words {
		display: flex;
		flex-direction: column;
		gap: 0.125rem;
		min-width: 0;
	}
	.yr-months__when {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.yr-months__album {
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.yr-months__repeat:hover .yr-months__album {
		color: var(--dn-title);
	}
</style>
