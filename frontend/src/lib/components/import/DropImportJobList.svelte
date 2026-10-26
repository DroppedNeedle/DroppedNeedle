<script lang="ts">
	import {
		CircleCheckBig,
		CircleAlert,
		CircleQuestionMark,
		LoaderCircle,
		SkipForward
	} from 'lucide-svelte';
	import { untrack } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
	import HeldEntryCard from '$lib/components/downloads/HeldEntryCard.svelte';
	import { getHeldEntriesQuery } from '$lib/queries/downloads/HeldQueries.svelte';
	import { getDropImportJobsQuery } from '$lib/queries/import/DropImportQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import type { DropImportItemStatus } from '$lib/queries/import/types';
	import type { HeldEntry } from '$lib/types';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		showAll?: boolean;
	}
	let { showAll = false }: Props = $props();

	const jobsQuery = getDropImportJobsQuery(
		() => authStore.isTrusted,
		() => showAll && authStore.isAdmin
	);
	const jobs = $derived(jobsQuery.data?.jobs ?? []);

	// An item waiting on a person is a held entry, shown with the queue's card.
	const reviewIds = $derived(
		jobs.flatMap((job) =>
			job.items.filter((item) => item.status === 'needs_review').map((item) => item.id)
		)
	);
	const heldQuery = getHeldEntriesQuery(
		() => ({ all: showAll && authStore.isAdmin }),
		() => authStore.isTrusted && reviewIds.length > 0
	);
	const entryByItem = $derived.by(() => {
		const byItem = new SvelteMap<number, HeldEntry>();
		for (const entry of heldQuery.data?.entries ?? []) {
			if (entry.drop_item_id != null) byItem.set(entry.drop_item_id, entry);
		}
		return byItem;
	});
	// An item that has just gone into review made a new entry: fetch the list again,
	// once for each new set of items in review.
	let fetchedFor = '';
	$effect(() => {
		const key = reviewIds.join(',');
		const missing = reviewIds.some((id) => !entryByItem.has(id));
		untrack(() => {
			if (!missing || key === fetchedFor || !heldQuery.isSuccess) return;
			fetchedFor = key;
			void heldQuery.refetch();
		});
	});

	const badges: Record<DropImportItemStatus, { label: string; cls: string }> = {
		processing: { label: 'Identifying…', cls: 'badge-info' },
		imported: { label: 'Imported', cls: 'badge-success' },
		skipped: { label: 'Skipped', cls: 'badge-ghost' },
		needs_review: { label: 'Needs a match', cls: 'badge-warning' },
		failed: { label: 'Failed', cls: 'badge-error' },
		discarded: { label: 'Discarded', cls: 'badge-ghost' }
	};
</script>

{#if jobsQuery.isLoading}
	<div class="space-y-3">
		<div class="skeleton h-16 w-full rounded-2xl"></div>
		<div class="skeleton h-16 w-full rounded-2xl"></div>
	</div>
{:else if jobs.length === 0}
	<p class="py-6 text-center text-sm text-base-content/50">
		Nothing imported yet. Drop a purchase above to get started.
	</p>
{:else}
	<div class="space-y-3">
		{#each jobs as job (job.id)}
			<Card variant="bordered" padding="md">
				<div>
					<div class="flex flex-wrap items-center justify-between gap-2">
						<div class="min-w-0">
							<p class="truncate text-sm font-semibold" title={job.upload_name}>
								{job.upload_name}
							</p>
							<p class="text-xs text-base-content/50">
								{formatLastUpdated(new Date(job.created_at * 1000))}
								{#if showAll && job.user_id !== authStore.user?.id}
									· by {job.user_name}
								{/if}
							</p>
						</div>
						{#if job.status === 'processing'}
							<span class="badge badge-info gap-1">
								<LoaderCircle class="h-3 w-3 animate-spin motion-keep" aria-hidden="true" /> Working…
							</span>
						{:else if job.status === 'failed'}
							<span class="badge badge-error" title={job.error ?? undefined}>Failed</span>
						{/if}
					</div>

					{#if job.error}
						<!-- a completed job carries notes here (a skipped corrupt archive),
					     a failed one carries the reason - only the latter is an error -->
						<p class="mt-2 text-xs {job.status === 'failed' ? 'text-error' : 'text-warning'}">
							{job.error}
						</p>
					{/if}

					{#if job.items.length}
						<ul class="mt-3 space-y-2">
							{#each job.items as item (item.id)}
								{@const badge = badges[item.status]}
								{@const entry =
									item.status === 'needs_review' ? entryByItem.get(item.id) : undefined}
								<li class="rounded-xl bg-base-100/60 px-3 py-2">
									<div class="min-w-0">
										<div class="flex min-w-0 items-center gap-2">
											{#if item.status === 'imported'}
												<CircleCheckBig class="h-4 w-4 shrink-0 text-success" aria-hidden="true" />
											{:else if item.status === 'needs_review'}
												<CircleQuestionMark
													class="h-4 w-4 shrink-0 text-warning"
													aria-hidden="true"
												/>
											{:else if item.status === 'failed'}
												<CircleAlert class="h-4 w-4 shrink-0 text-error" aria-hidden="true" />
											{:else if item.status === 'skipped' || item.status === 'discarded'}
												<SkipForward
													class="h-4 w-4 shrink-0 text-base-content/40"
													aria-hidden="true"
												/>
											{:else}
												<LoaderCircle
													class="h-4 w-4 shrink-0 animate-spin motion-keep text-info"
													aria-hidden="true"
												/>
											{/if}
											<p class="grow-0 truncate text-sm font-medium">
												{#if item.album_title}
													{item.artist_name ? `${item.artist_name} - ` : ''}{item.album_title}
												{:else}
													{item.folder_name}
												{/if}
											</p>
											<span class="badge badge-sm {badge.cls} shrink-0">{badge.label}</span>
										</div>
										{#if item.detail && !entry}
											<p class="mt-0.5 truncate text-xs text-base-content/50" title={item.detail}>
												{item.detail}
											</p>
										{/if}
										{#if item.reason_action && item.status !== 'imported' && !entry}
											<p
												class="mt-0.5 truncate text-xs text-base-content/40"
												data-testid="drop-import-reason-action"
												title={item.reason_action}
											>
												{item.reason_action}
											</p>
										{/if}
									</div>
									{#if entry}
										<div class="mt-2">
											<HeldEntryCard {entry} name={item.folder_name} compact />
										</div>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</div>
			</Card>
		{/each}
	</div>
{/if}
