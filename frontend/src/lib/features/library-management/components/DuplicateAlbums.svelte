<!--
	Albums the library lists more than once: two copies of one release,
	copies naming different releases of one group, and a copy with fewer
	tracks than a sibling. Each group shows its copies and the evidence
	behind them; every copy opens its album page. Nothing here deletes or
	merges.
-->
<script lang="ts">
	import { ArrowUpRight, CopyCheck } from 'lucide-svelte';

	import {
		getDuplicateAlbumsQuery,
		type DuplicateClass,
		type DuplicateCopy,
		type DuplicateDecisionSource,
		type DuplicateEvidence
	} from '../queries/DuplicateQueries.svelte';
	import { formatBytes } from '$lib/utils/formatting';
	import { albumHref } from '$lib/utils/entityRoutes';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import PageShell from '$lib/ui/PageShell.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';

	const duplicates = getDuplicateAlbumsQuery();
	const groups = $derived(duplicates.data?.pages.flatMap((page) => page.items) ?? []);

	const classLabels: Record<DuplicateClass, string> = {
		same_release: 'Same release',
		different_releases: 'Different releases',
		incomplete_copy: 'Missing tracks'
	};

	const sourceLabels: Record<DuplicateDecisionSource, string> = {
		automatic: 'found automatically',
		manual: 'chosen by hand',
		legacy_import: 'carried from v2',
		embedded: 'taken from the tags'
	};

	function classTone(kind: DuplicateClass): 'warning' | 'info' | 'neutral' {
		if (kind === 'incomplete_copy') return 'warning';
		return kind === 'different_releases' ? 'info' : 'neutral';
	}

	function tracksLine(copy: DuplicateCopy): string {
		const formats = copy.formats.length ? copy.formats.join(', ') : 'unknown format';
		return `${copy.track_count} tracks · ${formats} · ${formatBytes(copy.total_size_bytes)}`;
	}

	function evidenceLine(copy: DuplicateCopy, evidence: DuplicateEvidence): string {
		const matched = copy.decision_source ? ` (${sourceLabels[copy.decision_source]})` : '';
		if (copy.release_mbid) return `Release ${copy.release_mbid}${matched}`;
		return evidence === 'stored_match'
			? `Its match names no release${matched}`
			: "Its files' tags name this group";
	}
</script>

<PageShell
	title="Duplicates"
	subtitle="Albums the library lists more than once. Nothing is deleted or merged here; open a copy to see what it holds."
>
	<QueryBoundary query={duplicates} what="the duplicates list" isEmpty={() => groups.length === 0}>
		{#snippet empty()}
			<Card variant="bordered" padding="lg">
				<CopyCheck class="mx-auto h-8 w-8 text-success" />
				<p class="mt-3 text-center text-muted">No album is listed twice.</p>
			</Card>
		{/snippet}
		{#snippet children(data)}
			<ul class="flex flex-col gap-3" aria-label="Duplicate groups">
				{#each data.pages.flatMap((page) => page.items) as group (group.id)}
					<li>
						<Card variant="bordered" padding="md">
							<header class="flex flex-wrap items-start justify-between gap-3">
								<div class="min-w-0">
									<p class="font-mono text-xs uppercase tracking-wide text-muted">Release group</p>
									<p class="break-all font-mono text-sm">{group.id}</p>
									<p class="mt-1 text-sm text-muted">
										{group.copies.length} copies{#if group.release_mbid}
											· one release: <span class="break-all font-mono text-xs"
												>{group.release_mbid}</span
											>{/if}
									</p>
								</div>
								<div class="flex flex-wrap gap-2">
									{#each group.classes as kind (kind)}
										<StatusChip tone={classTone(kind)}>{classLabels[kind]}</StatusChip>
									{/each}
								</div>
							</header>
							<ul class="mt-3 flex flex-col gap-2">
								{#each group.copies as copy (copy.album_id)}
									<li class="rounded-lg border border-base-content/10 p-3">
										<div class="flex flex-wrap items-start justify-between gap-3">
											<div class="min-w-0">
												<a class="link-hover font-semibold" href={albumHref(copy.album_id)}
													>{copy.title}</a
												>
												<p class="truncate text-sm text-muted">{copy.artist}</p>
												<p class="mt-1 text-sm">{tracksLine(copy)}</p>
												<p class="mt-1 break-all text-xs text-muted">
													{evidenceLine(copy, copy.evidence)}
												</p>
												{#if copy.doubted}
													<p class="mt-1 text-xs text-warning">
														Its stored match is a guess the files contradict.
													</p>
												{/if}
											</div>
											<IconButton
												size="xs"
												shape="square"
												icon={ArrowUpRight}
												label={`Open ${copy.title}`}
												href={albumHref(copy.album_id)}
											/>
										</div>
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
						onclick={() => duplicates.fetchNextPage()}>Show more</Button
					>
				</div>
			{/if}
		{/snippet}
	</QueryBoundary>
</PageShell>
