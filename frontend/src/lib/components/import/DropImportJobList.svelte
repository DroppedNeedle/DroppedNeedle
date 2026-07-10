<script lang="ts">
	import {
		CheckCircle2,
		CircleAlert,
		CircleHelp,
		Loader2,
		SkipForward,
		Trash2
	} from 'lucide-svelte';
	import DropImportMatchModal from './DropImportMatchModal.svelte';
	import { getDropImportJobsQuery } from '$lib/queries/import/DropImportQueries.svelte';
	import { discardDropItemMutation } from '$lib/queries/import/DropImportMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import type { DropImportItem, DropImportItemStatus } from '$lib/queries/import/types';

	interface Props {
		showAll?: boolean;
	}
	let { showAll = false }: Props = $props();

	const jobsQuery = getDropImportJobsQuery(
		() => authStore.isTrusted,
		() => showAll && authStore.isAdmin
	);
	const discard = discardDropItemMutation();
	const jobs = $derived(jobsQuery.data?.jobs ?? []);

	let matching = $state<DropImportItem | null>(null);

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
			<div class="rounded-2xl border border-base-content/10 bg-base-200/40 p-4">
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
							<Loader2 class="h-3 w-3 animate-spin" aria-hidden="true" /> Working…
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
							<li
								class="flex flex-wrap items-center justify-between gap-2 rounded-xl bg-base-100/60 px-3 py-2"
							>
								<div class="min-w-0 flex-1">
									<div class="flex min-w-0 items-center gap-2">
										{#if item.status === 'imported'}
											<CheckCircle2 class="h-4 w-4 shrink-0 text-success" aria-hidden="true" />
										{:else if item.status === 'needs_review'}
											<CircleHelp class="h-4 w-4 shrink-0 text-warning" aria-hidden="true" />
										{:else if item.status === 'failed'}
											<CircleAlert class="h-4 w-4 shrink-0 text-error" aria-hidden="true" />
										{:else if item.status === 'skipped' || item.status === 'discarded'}
											<SkipForward
												class="h-4 w-4 shrink-0 text-base-content/40"
												aria-hidden="true"
											/>
										{:else}
											<Loader2 class="h-4 w-4 shrink-0 animate-spin text-info" aria-hidden="true" />
										{/if}
										<p class="truncate text-sm font-medium">
											{#if item.album_title}
												{item.artist_name ? `${item.artist_name} - ` : ''}{item.album_title}
											{:else}
												{item.folder_name}
											{/if}
										</p>
										<span class="badge badge-sm {badge.cls} shrink-0">{badge.label}</span>
									</div>
									{#if item.detail}
										<p class="mt-0.5 truncate text-xs text-base-content/50" title={item.detail}>
											{item.detail}
										</p>
									{/if}
								</div>
								{#if item.status === 'needs_review'}
									<div class="flex shrink-0 items-center gap-1">
										<button class="btn btn-primary btn-xs" onclick={() => (matching = item)}>
											Match…
										</button>
										<button
											class="btn btn-ghost btn-xs"
											onclick={() => discard.mutate(item.id)}
											disabled={discard.isPending}
											aria-label="Discard {item.folder_name}"
										>
											<Trash2 class="h-3.5 w-3.5" aria-hidden="true" />
										</button>
									</div>
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
			</div>
		{/each}
	</div>
{/if}

{#if matching}
	<DropImportMatchModal item={matching} onclose={() => (matching = null)} />
{/if}
