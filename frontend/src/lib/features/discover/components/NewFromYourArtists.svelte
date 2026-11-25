<!--
	New from your artists: the arrivals sheet of releases from artists the
	person plays, owns or follows, newest first, on a flat dark panel. Each
	row has its release day in a gutter, the cover, title and artist, its
	type, the line saying why for an artist close to theirs, and Owned, a
	Requested chip or Keep (an album request made from the list). With
	`href` a head names the sheet, says how old the list is and links to the
	whole list.
-->
<script lang="ts">
	import { Check } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { libraryStore } from '$lib/stores/library';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import type { NewRelease } from '../queries/newReleases.svelte';

	interface Props {
		releases: NewRelease[];
		/** When ListenBrainz's list was read (unix seconds), for the head. */
		updatedAt?: number | null;
		/** The whole list's page; with it the sheet has its head. */
		href?: string | null;
	}

	let { releases, updatedAt = null, href = null }: Props = $props();

	const request = createRequestAlbumMutation();
	let keeping = $state<string | null>(null);

	const TYPES = { album: 'Album', ep: 'EP', single: 'Single', other: 'Other' } as const;
	const KINDS = { live: 'Live', demo: 'Demo', compilation: 'Compilation' } as const;

	// A release date is a calendar day: read it as one, never shifted by the
	// browser's zone.
	function dateOf(row: NewRelease): Date | null {
		const day = row.album.release_date;
		if (!day || !/^\d{4}-\d{2}-\d{2}$/.test(day)) return null;
		return new Date(`${day}T00:00:00Z`);
	}

	function day(at: Date) {
		return at.toLocaleDateString(undefined, { day: '2-digit', timeZone: 'UTC' });
	}

	function month(at: Date) {
		return at.toLocaleDateString(undefined, { month: 'short', timeZone: 'UTC' });
	}

	const updated = $derived(
		updatedAt
			? new Date(updatedAt * 1000).toLocaleDateString(undefined, {
					day: 'numeric',
					month: 'short'
				})
			: null
	);

	// "Artist - Type - Kind", each part left out when it is missing.
	const byline = (row: NewRelease) =>
		[
			row.album.artist_name,
			row.album.release_type && TYPES[row.album.release_type],
			row.secondary_kind && KINDS[row.secondary_kind]
		]
			.filter(Boolean)
			.join(' - ');

	const requested = (row: NewRelease) =>
		row.album.requested || libraryStore.isRequested(row.album.musicbrainz_id);

	async function keep(row: NewRelease) {
		const mbid = row.album.musicbrainz_id;
		if (!mbid || keeping) return;
		keeping = mbid;
		try {
			await request.mutateAsync({
				release_group_mbid: mbid,
				artist_name: row.album.artist_name ?? undefined,
				album_title: row.album.title,
				year: row.album.year ?? undefined,
				artist_mbid: row.album.artist_id ?? undefined,
				made_from: 'new_releases'
			});
		} catch {
			// the request's own toast says what went wrong
		} finally {
			keeping = null;
		}
	}
</script>

{#if href}
	<SectionHead title="New from your artists" {href} moreLabel="All">
		{#snippet chips()}
			<span class="dn-arrivals__when">
				Last 90 days{#if updated}&nbsp;- updated {updated}{/if}
			</span>
		{/snippet}
	</SectionHead>
{/if}

<Card variant="bordered" padding="none">
	<ul aria-label="New from your artists">
		{#each releases as row (row.album.musicbrainz_id)}
			{@const mbid = row.album.musicbrainz_id ?? ''}
			{@const at = dateOf(row)}
			<li class="dn-arrivals__row">
				<div class="dn-arrivals__date">
					{#if at}
						<span>
							<b>{day(at)}</b>
							{month(at)}
						</span>
					{/if}
				</div>
				<a class="dn-arrivals__cover" href={albumHref(mbid)} tabindex="-1" aria-hidden="true">
					<AlbumImage
						{mbid}
						customUrl={row.album.cover_url ?? null}
						alt=""
						size="full"
						requestSize={250}
						lazy
						rounded="none"
						className="aspect-square w-full"
					/>
				</a>
				<div class="min-w-0">
					<a class="dn-arrivals__title" href={albumHref(mbid)}>{row.album.title}</a>
					<p class="dn-arrivals__artist">
						{byline(row)}
					</p>
					{#if row.reason}<p class="dn-arrivals__reason">{row.reason}</p>{/if}
				</div>
				<div class="dn-arrivals__actions">
					{#if row.album.in_library}
						<StatusChip icon={Check}>Owned</StatusChip>
					{:else if requested(row)}
						<StatusChip look="waiting">Requested</StatusChip>
					{:else}
						<Button
							variant="outline"
							size="sm"
							loading={keeping === mbid}
							aria-label="Keep {row.album.title}"
							onclick={() => keep(row)}
						>
							Keep
						</Button>
					{/if}
				</div>
			</li>
		{/each}
	</ul>
</Card>

<style>
	.dn-arrivals__when {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-arrivals__row {
		display: grid;
		grid-template-columns: 4.625rem 3rem minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.875rem;
		padding: 0.5625rem 1rem 0.5625rem 0;
		border-top: 1px solid color-mix(in oklab, var(--dn-line) 50%, transparent);
	}
	.dn-arrivals__row:first-child {
		border-top: 0;
	}
	.dn-arrivals__date {
		align-self: stretch;
		display: grid;
		place-items: center;
		margin: -0.5625rem 0;
		border-right: 1px dashed var(--dn-line);
		font-family: var(--font-mono);
		font-size: 0.625rem;
		font-weight: 600;
		line-height: 1.2;
		letter-spacing: 0.12em;
		text-align: center;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-arrivals__date b {
		display: block;
		font-family: var(--font-display);
		font-size: 1.375rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.02em;
		color: var(--color-base-content);
	}
	.dn-arrivals__cover {
		display: block;
		width: 3rem;
	}
	.dn-arrivals__title {
		display: block;
		overflow: hidden;
		font-size: 0.9375rem;
		font-weight: 600;
		white-space: nowrap;
		text-overflow: ellipsis;
	}
	.dn-arrivals__title:hover {
		text-decoration: underline;
	}
	.dn-arrivals__artist {
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.08em;
		white-space: nowrap;
		text-overflow: ellipsis;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-arrivals__reason {
		margin-top: 0.125rem;
		overflow: hidden;
		font-size: 0.8125rem;
		font-weight: 500;
		white-space: nowrap;
		text-overflow: ellipsis;
		color: var(--dn-muted);
	}
	.dn-arrivals__actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.5rem;
	}
	@media (max-width: 40rem) {
		.dn-arrivals__row {
			grid-template-columns: 3.5rem 3rem minmax(0, 1fr);
			row-gap: 0.5rem;
		}
		.dn-arrivals__date {
			grid-row: span 2;
		}
		.dn-arrivals__actions {
			grid-column: 2 / -1;
			justify-content: flex-start;
		}
	}
</style>
