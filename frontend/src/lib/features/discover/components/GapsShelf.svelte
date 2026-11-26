<!--
	Gaps in artists you collect: each gap on a flat dark panel, drawn as the
	artist's shelf with a hole in it (the albums the library holds, in date
	order with their years, and a dashed slot where the missing one falls),
	beside it the missing album's name, its artist and year, the line saying
	how many of their albums the library holds, and Keep (an album request
	made from the shelf), or a Requested chip.
-->
<script lang="ts">
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import GapShelf from '$lib/ui/GapShelf.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { libraryStore } from '$lib/stores/library';
	import { createRequestAlbumMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import type { AlbumRef, Gap } from '../queries/types';

	interface Props {
		gaps: Gap[];
	}

	let { gaps }: Props = $props();

	const request = createRequestAlbumMutation();
	let keeping = $state<string | null>(null);

	const requested = (gap: Gap) =>
		gap.album.requested || libraryStore.isRequested(gap.album.musicbrainz_id);

	const heldOf = (gap: Gap) =>
		gap.held.map((album: AlbumRef, at: number) => ({
			key: album.local_id ?? album.musicbrainz_id ?? `${at}`,
			year: album.year ?? null,
			album
		}));

	const byline = (gap: Gap) =>
		[gap.album.artist_name, gap.album.year]
			.filter((part) => part != null && part !== '')
			.join(' - ');

	async function keep(gap: Gap) {
		const mbid = gap.album.musicbrainz_id;
		if (!mbid || keeping) return;
		keeping = mbid;
		try {
			await request.mutateAsync({
				release_group_mbid: mbid,
				artist_name: gap.album.artist_name ?? undefined,
				album_title: gap.album.title,
				year: gap.album.year ?? undefined,
				artist_mbid: gap.album.artist_id ?? undefined,
				made_from: 'discover_shelf'
			});
		} catch {
			// the request's own toast says what went wrong
		} finally {
			keeping = null;
		}
	}
</script>

<SectionHead title="Gaps in artists you collect" />

<ul class="dn-gaps" aria-label="Gaps in artists you collect">
	{#each gaps as gap (gap.album.musicbrainz_id)}
		{@const mbid = gap.album.musicbrainz_id ?? ''}
		<li>
			<Card variant="bordered" padding="none" class="overflow-hidden">
				<div class="dn-gaps__card">
					<GapShelf
						class="dn-gaps__shelf"
						items={heldOf(gap)}
						year={gap.album.year ?? null}
						label="Their albums on your shelves, with a space where {gap.album.title} would go"
					>
						{#snippet cover(item)}
							<AlbumImage
								mbid={item.album.musicbrainz_id ?? ''}
								albumId={item.album.musicbrainz_id ? undefined : (item.album.local_id ?? undefined)}
								source={item.album.musicbrainz_id ? 'provider' : 'local'}
								alt=""
								size="full"
								requestSize={250}
								lazy
								rounded="none"
								className="aspect-square w-full"
							/>
						{/snippet}
					</GapShelf>
					<div class="dn-gaps__name">
						<a class="dn-gaps__title" href={albumHref(mbid)}>{gap.album.title}</a>
						<p class="dn-gaps__by">{byline(gap)}</p>
						<p class="dn-gaps__reason">{gap.reason}</p>
					</div>
					<div class="dn-gaps__actions">
						{#if requested(gap)}
							<StatusChip look="waiting">Requested</StatusChip>
						{:else}
							<Button
								variant="outline"
								size="sm"
								loading={keeping === mbid}
								aria-label="Keep {gap.album.title}"
								onclick={() => keep(gap)}
							>
								Keep
							</Button>
						{/if}
					</div>
				</div>
			</Card>
		</li>
	{/each}
</ul>

<style>
	.dn-gaps {
		display: flex;
		flex-direction: column;
		gap: 0.875rem;
	}
	.dn-gaps__card {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: 1.125rem;
		padding: 0.875rem 1.125rem 0;
	}
	.dn-gaps :global(.dn-gaps__shelf) {
		align-self: end;
	}
	.dn-gaps__name {
		min-width: 0;
		padding-bottom: 0.75rem;
	}
	.dn-gaps__title {
		display: block;
		overflow: hidden;
		font-family: var(--font-display);
		font-size: 1.6875rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-gaps__title:hover {
		text-decoration: underline;
	}
	.dn-gaps__by {
		margin-top: 0.3125rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--dn-muted);
	}
	.dn-gaps__reason {
		margin-top: 0.5rem;
		font-size: 0.875rem;
	}
	.dn-gaps__actions {
		padding-bottom: 0.75rem;
	}
	@media (max-width: 40rem) {
		.dn-gaps__card {
			grid-template-columns: minmax(0, 1fr) auto;
			padding: 0.875rem 1rem;
		}
		.dn-gaps :global(.dn-gaps__shelf) {
			grid-column: 1 / -1;
			order: 2;
		}
		.dn-gaps__name,
		.dn-gaps__actions {
			padding-bottom: 0;
		}
	}
</style>
