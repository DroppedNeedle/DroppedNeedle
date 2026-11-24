<!--
	The caller's Later pile: the albums they put aside, newest first, as rows
	on a flat dark sheet. Each row has its day on the pile, the cover, title
	and artist, a sample, Keep (an album request made from the pile) and
	Remove. "Keep all" opens the batch dialog with the newest albums on the
	pile that can still be requested, as many as one batch takes. An album the
	library comes to own leaves the pile by itself.
-->
<script lang="ts">
	import { Clock, Download, X } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import EmptyState from '$lib/ui/EmptyState.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { libraryStore } from '$lib/stores/library';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { applyEffects, laterChanged } from '$lib/queries/effects';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import DiscoveryBatchModal from './DiscoveryBatchModal.svelte';
	import SampleButton from './SampleButton.svelte';
	import {
		createRemoveLaterMutation,
		getLaterPileQuery,
		type LaterAlbum
	} from '../queries/later.svelte';

	const pile = getLaterPileQuery();
	const remove = createRemoveLaterMutation();
	const request = createRequestAlbumMutation();

	// The most albums one batch takes: acquire's BATCH_MAX_ITEMS, which the
	// server enforces.
	const KEEP_ALL_MAX = 30;

	let batchOpen = $state(false);
	let keeping = $state<string | null>(null);

	const rows = $derived(pile.data?.items ?? []);
	const total = $derived(pile.data?.total ?? rows.length);

	function day(at: number) {
		return new Date(at * 1000).toLocaleDateString(undefined, { day: '2-digit' });
	}

	function month(at: number) {
		return new Date(at * 1000).toLocaleDateString(undefined, { month: 'short' });
	}

	const requested = (row: LaterAlbum) =>
		row.album.requested || libraryStore.isRequested(row.album.musicbrainz_id);

	// What Keep all offers: the newest albums still to request, one batch's worth.
	const requestable = $derived(rows.filter((row) => !row.album.in_library && !requested(row)));
	const keepAll = $derived(requestable.slice(0, KEEP_ALL_MAX).map((row) => row.album));

	async function keep(row: LaterAlbum) {
		const mbid = row.album.musicbrainz_id;
		if (!mbid || keeping) return;
		keeping = mbid;
		try {
			await request.mutateAsync({
				release_group_mbid: mbid,
				artist_name: row.album.artist_name ?? undefined,
				album_title: row.album.title,
				artist_mbid: row.album.artist_id ?? undefined,
				made_from: 'later'
			});
			await applyEffects(laterChanged(authStore.user?.id));
		} catch {
			// the request's own toast says what went wrong
		} finally {
			keeping = null;
		}
	}
</script>

<QueryBoundary query={pile} what="your Later pile">
	{#snippet loading()}
		<div class="dn-later" aria-hidden="true">
			{#each Array(4) as _, i (i)}
				<div class="dn-later__row">
					<div class="skeleton dn-later__date rounded-none"></div>
					<div class="skeleton size-12 rounded-none"></div>
					<div class="flex flex-col gap-2">
						<div class="skeleton h-4 w-40"></div>
						<div class="skeleton h-3 w-24"></div>
					</div>
				</div>
			{/each}
		</div>
	{/snippet}
	{#snippet empty()}
		<EmptyState
			icon={Clock}
			title="Nothing on your Later pile"
			description="Choose Later on an album you don't own to keep it here."
			ctaLabel="Back to Discover"
			ctaHref="/discover"
		/>
	{/snippet}
	{#snippet children(data)}
		<div class="dn-later__bar">
			<p class="dn-later__count">
				{total}
				{total === 1 ? 'album' : 'albums'} for later
			</p>
			{#if keepAll.length > 0}
				<div class="dn-later__keep">
					{#if requestable.length > KEEP_ALL_MAX}
						<span class="dn-later__note">Keep all takes the newest {KEEP_ALL_MAX}.</span>
					{/if}
					<Button variant="primary" size="sm" icon={Download} onclick={() => (batchOpen = true)}>
						Keep all
					</Button>
				</div>
			{/if}
		</div>
		<ul class="dn-later">
			{#each data.items as row (row.album.musicbrainz_id)}
				{@const mbid = row.album.musicbrainz_id ?? ''}
				<li class="dn-later__row">
					<div class="dn-later__date">
						<span>
							<b>{day(row.added_at)}</b>
							{month(row.added_at)}
						</span>
					</div>
					<a class="dn-later__cover" href={albumHref(mbid)} tabindex="-1" aria-hidden="true">
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
						<a class="dn-later__title" href={albumHref(mbid)}>{row.album.title}</a>
						<p class="dn-later__artist">{row.album.artist_name}</p>
					</div>
					<div class="dn-later__actions">
						<SampleButton
							sampleKey={mbid}
							artist={row.album.artist_name ?? ''}
							title={row.album.title}
							albumMbid={mbid}
							artistMbid={row.album.artist_id ?? null}
							coverUrl={row.album.cover_url ?? null}
						/>
						{#if requested(row)}
							<StatusChip look="waiting" />
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
						<Button
							variant="ghost"
							size="sm"
							icon={X}
							disabled={remove.isPending && remove.variables === mbid}
							aria-label="Remove {row.album.title} from your Later pile"
							onclick={() => remove.mutate(mbid)}
						>
							Remove
						</Button>
					</div>
				</li>
			{/each}
		</ul>
		{#if total > data.items.length}
			<p class="dn-later__more">Showing the newest {data.items.length} of {total}.</p>
		{/if}
	{/snippet}
</QueryBoundary>

<DiscoveryBatchModal
	bind:open={batchOpen}
	sectionTitle="Later"
	sectionKey="later"
	madeFrom="later"
	albums={keepAll}
/>

<style>
	.dn-later__bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		margin-bottom: 1rem;
	}
	.dn-later__count {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.dn-later__keep {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
	}
	.dn-later__note {
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	.dn-later {
		overflow: hidden;
		border: 1px solid var(--dn-line);
		border-radius: 0.75rem;
		background-color: var(--color-base-200);
	}
	.dn-later__row {
		display: grid;
		grid-template-columns: 4.625rem 3rem minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.875rem;
		padding: 0.5625rem 1rem 0.5625rem 0;
		border-top: 1px solid color-mix(in oklab, var(--dn-line) 50%, transparent);
	}
	.dn-later__row:first-child {
		border-top: 0;
	}
	.dn-later__date {
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
	.dn-later__date b {
		display: block;
		font-family: var(--font-display);
		font-size: 1.375rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.02em;
		color: var(--color-base-content);
	}
	.dn-later__cover {
		display: block;
		width: 3rem;
	}
	.dn-later__title {
		display: block;
		overflow: hidden;
		font-size: 0.9375rem;
		font-weight: 600;
		white-space: nowrap;
		text-overflow: ellipsis;
	}
	.dn-later__title:hover {
		text-decoration: underline;
	}
	.dn-later__artist {
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
	.dn-later__actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 0.5rem;
	}
	.dn-later__more {
		margin-top: 0.75rem;
		font-size: 0.875rem;
		color: var(--dn-muted);
	}
	@media (max-width: 40rem) {
		.dn-later__row {
			grid-template-columns: 3.5rem 3rem minmax(0, 1fr);
			row-gap: 0.5rem;
		}
		.dn-later__date {
			grid-row: span 2;
		}
		.dn-later__actions {
			grid-column: 2 / -1;
			justify-content: flex-start;
			flex-wrap: wrap;
		}
	}
</style>
