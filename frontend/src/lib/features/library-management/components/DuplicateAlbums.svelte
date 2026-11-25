<!--
	Albums the library holds more than once: every release group two or more
	albums own, marked when two copies share a release, the copies name
	different releases, or one has fewer tracks. Each copy opens its album
	page. Nothing here deletes or merges.
-->
<script lang="ts">
	import { CopyCheck } from 'lucide-svelte';

	import {
		getDuplicateAlbumsQuery,
		type DuplicateClass,
		type DuplicateCopy,
		type DuplicateDecisionSource,
		type DuplicateGroup
	} from '../queries/DuplicateQueries.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import { albumHref } from '$lib/utils/entityRoutes';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import PageShell from '$lib/ui/PageShell.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';

	const duplicates = getDuplicateAlbumsQuery();
	const groups = $derived(duplicates.data?.pages.flatMap((page) => page.items) ?? []);

	const classLabels: Record<DuplicateClass, string> = {
		same_release: 'Same release',
		different_releases: 'Different releases',
		incomplete_copy: 'Fewer tracks'
	};

	const sourceLabels: Record<DuplicateDecisionSource, string> = {
		automatic: 'found automatically',
		manual: 'chosen by hand',
		legacy_import: 'carried from v2',
		embedded: 'from the tags'
	};

	function classTone(kind: DuplicateClass): 'warning' | 'info' | 'neutral' {
		if (kind === 'incomplete_copy') return 'warning';
		return kind === 'different_releases' ? 'info' : 'neutral';
	}

	function mostTracks(group: DuplicateGroup): number {
		return Math.max(...group.copies.map((copy) => copy.track_count));
	}

	function tracksLine(copy: DuplicateCopy): string {
		const tracks = copy.track_count === 1 ? '1 track' : `${copy.track_count} tracks`;
		return `${tracks} · ${formatBytes(copy.total_size_bytes)}`;
	}

	function evidenceLine(copy: DuplicateCopy): string {
		if (copy.evidence === 'file_tags') return 'Grouped by its tags';
		return copy.decision_source ? `Matched, ${sourceLabels[copy.decision_source]}` : 'Matched';
	}
</script>

<PageShell title="Duplicates" subtitle="Albums you have more than once. Nothing here changes them.">
	<QueryBoundary query={duplicates} what="the duplicates list" isEmpty={() => groups.length === 0}>
		{#snippet empty()}
			<Card variant="bordered" padding="lg">
				<CopyCheck class="mx-auto h-8 w-8 text-success" />
				<p class="mt-3 text-center text-muted">No album is listed twice.</p>
			</Card>
		{/snippet}
		<ul class="flex flex-col gap-3" aria-label="Duplicate groups">
			{#each groups as group (group.id)}
				{@const most = mostTracks(group)}
				<li>
					<Card
						variant="bordered"
						padding="md"
						as="section"
						aria-labelledby={`duplicate-${group.id}`}
					>
						{#snippet header()}
							<div class="flex flex-wrap items-start justify-between gap-3">
								<div class="min-w-0">
									<CardTitle id={`duplicate-${group.id}`}>{group.title}</CardTitle>
									<p class="truncate text-sm text-muted">
										{group.artist} · {group.copies.length} copies
									</p>
								</div>
								<div class="flex flex-wrap gap-2">
									{#each group.classes as kind (kind)}
										<StatusChip tone={classTone(kind)}>{classLabels[kind]}</StatusChip>
									{/each}
								</div>
							</div>
						{/snippet}
						<ul class="mt-2 grid gap-2 sm:grid-cols-2">
							{#each group.copies as copy (copy.album_id)}
								<li>
									<Card
										variant="flat"
										padding="sm"
										href={albumHref(copy.album_id)}
										aria-label={`Open ${copy.title}`}
									>
										<div class="flex min-w-0 items-start gap-3">
											<div class="size-14 shrink-0 overflow-hidden rounded-lg">
												<AlbumImage
													albumId={copy.album_id}
													mbid={group.id}
													available={copy.cover_available}
													alt={copy.title}
													size="full"
													requestSize={250}
													rounded="none"
													className="h-full w-full"
												/>
											</div>
											<div class="min-w-0 flex-1">
												<p class="truncate font-semibold">{copy.title}</p>
												<p class="truncate text-sm text-muted">{copy.artist}</p>
												<p class="mt-1 text-sm">{tracksLine(copy)}</p>
												<div class="mt-1 flex flex-wrap gap-1">
													{#each copy.formats as format (format)}
														<StatusChip size="xs">{format.toUpperCase()}</StatusChip>
													{/each}
													{#if copy.track_count < most}
														<StatusChip size="xs" tone="warning">Fewer tracks</StatusChip>
													{/if}
													{#if copy.doubted}
														<StatusChip size="xs" tone="warning">Guess</StatusChip>
													{/if}
												</div>
												<p class="mt-1 break-all text-xs text-muted">{evidenceLine(copy)}</p>
											</div>
										</div>
									</Card>
								</li>
							{/each}
						</ul>
					</Card>
				</li>
			{/each}
		</ul>
		{#if duplicates.hasNextPage}
			<div class="mt-4 flex justify-center">
				<Button
					variant="ghost"
					loading={duplicates.isFetchingNextPage}
					onclick={() => duplicates.fetchNextPage()}
				>
					Show more
				</Button>
			</div>
		{/if}
	</QueryBoundary>
</PageShell>
