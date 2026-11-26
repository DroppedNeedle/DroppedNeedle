<!--
	From your shelves: the person's own albums in two titled lists on flat
	dark panels, those played a lot but not lately and those asked for and
	never played. Each has its cover with Play, its title and artist, the
	line built from its own numbers and, for one played, the quiet gauge
	from the first month of the person's history to this month. A list with
	nothing in it says so in one line.
-->
<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import AlbumCardOverlay from '$lib/features/album/components/AlbumCardOverlay.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QuietGauge from '$lib/ui/QuietGauge.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import { albumHref, localAlbumHref } from '$lib/utils/entityRoutes';
	import type { OwnShelfAlbum, OwnShelves } from '../queries/types';

	interface Props {
		shelves: OwnShelves;
	}

	let { shelves }: Props = $props();

	const MONTHS = [
		'January',
		'February',
		'March',
		'April',
		'May',
		'June',
		'July',
		'August',
		'September',
		'October',
		'November',
		'December'
	] as const;

	function index(month: string): number | null {
		const match = /^(\d{4})-(\d{2})$/.exec(month);
		return match ? Number(match[1]) * 12 + Number(match[2]) - 1 : null;
	}

	// "Last played in February, quiet for 9 months", from the row's months.
	function quietLabel(last: string): string {
		const [at, now] = [index(last), index(shelves.month)];
		if (at === null || now === null) return 'Last played';
		const name = MONTHS[at % 12];
		const sameYear = Math.floor(at / 12) === Math.floor(now / 12);
		const when = sameYear ? name : `${name} ${Math.floor(at / 12)}`;
		const quiet = now - at;
		if (quiet <= 0) return `Last played in ${when}`;
		return `Last played in ${when}, quiet for ${quiet} ${quiet === 1 ? 'month' : 'months'}`;
	}

	const lists = $derived([
		{
			id: 'played',
			title: 'Played a lot, not lately',
			rows: shelves.played_a_lot,
			none: 'Nothing you played a lot has gone quiet.'
		},
		{
			id: 'never',
			title: 'Asked for, never played',
			rows: shelves.never_played,
			none: 'Nothing you asked for is waiting to be played.'
		}
	]);

	const hrefOf = (row: OwnShelfAlbum) =>
		row.album.musicbrainz_id
			? albumHref(row.album.musicbrainz_id)
			: row.album.local_id
				? localAlbumHref(row.album.local_id)
				: undefined;
</script>

<SectionHead title="From your shelves" />

{#each lists as list (list.id)}
	<section class="dn-own" aria-labelledby="dn-own-{list.id}">
		<h3 id="dn-own-{list.id}" class="dn-own__title">{list.title}</h3>
		{#if list.rows.length === 0}
			<p class="dn-own__none">{list.none}</p>
		{:else}
			<ul class="dn-own__grid" aria-labelledby="dn-own-{list.id}">
				{#each list.rows as row (row.album.local_id)}
					{@const mbid = row.album.musicbrainz_id ?? ''}
					<li>
						<Card variant="bordered" padding="md" class="h-full">
							<div class="dn-own__card">
								<div class="dn-own__sleeve">
									<AlbumImage
										{mbid}
										albumId={mbid ? undefined : (row.album.local_id ?? undefined)}
										source={mbid ? 'provider' : 'local'}
										alt="Cover of {row.album.title}"
										size="full"
										requestSize={250}
										lazy
										rounded="none"
										className="aspect-square w-full"
									/>
									{#if mbid}
										<AlbumCardOverlay
											{mbid}
											albumName={row.album.title}
											artistName={row.album.artist_name ?? 'Unknown'}
											coverUrl={row.album.cover_url ?? null}
											artistId={row.album.artist_id ?? undefined}
											size="sm"
											playShown
										/>
									{/if}
								</div>
								<div class="min-w-0">
									<a class="dn-own__album" href={hrefOf(row)}>{row.album.title}</a>
									{#if row.album.artist_name}
										<p class="dn-own__artist">{row.album.artist_name}</p>
									{/if}
									<p class="dn-own__line">{row.line}</p>
									{#if row.last_played}
										<QuietGauge
											class="dn-own__gauge"
											from={shelves.history_from ?? row.last_played}
											last={row.last_played}
											now={shelves.month}
											label={quietLabel(row.last_played)}
										/>
									{/if}
								</div>
							</div>
						</Card>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
{/each}

<style>
	.dn-own + .dn-own {
		margin-top: 1.5rem;
	}
	.dn-own__title {
		margin-bottom: 0.75rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-own__none {
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	.dn-own__grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(100%, 22rem), 1fr));
		gap: 1.375rem;
	}
	.dn-own__card {
		display: grid;
		grid-template-columns: 9.25rem minmax(0, 1fr);
		gap: 1.125rem;
	}
	.dn-own__sleeve {
		position: relative;
		width: 9.25rem;
		align-self: start;
		aspect-ratio: 1;
		box-shadow: 4px 10px 18px -10px rgb(0 0 0 / 0.9);
	}
	.dn-own__album {
		display: block;
		margin-top: 0.125rem;
		font-size: 1.0625rem;
		font-weight: 600;
		line-height: 1.2;
	}
	.dn-own__album:hover {
		text-decoration: underline;
	}
	.dn-own__artist {
		margin-top: 0.1875rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-own__line {
		margin-top: 0.75rem;
		font-size: 0.90625rem;
		font-weight: 500;
	}
	.dn-own__grid :global(.dn-own__gauge) {
		margin-top: 0.625rem;
	}
	@media (max-width: 40rem) {
		.dn-own__card {
			grid-template-columns: 6rem minmax(0, 1fr);
		}
		.dn-own__sleeve {
			width: 6rem;
		}
	}
</style>
