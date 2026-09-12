<script lang="ts">
	import { ChevronDown, LoaderCircle, Sparkles, Trash2 } from 'lucide-svelte';
	import { slide } from 'svelte/transition';
	import { SvelteSet } from 'svelte/reactivity';
	import {
		getDiscoveryBatchesQuery,
		removeDiscoveryBatch
	} from '$lib/queries/discover/DiscoveryBatchQuery.svelte';
	import { api } from '$lib/api/client';
	import { API } from '$lib/constants';
	import { albumHrefOrNull } from '$lib/utils/entityRoutes';
	import type { DiscoveryBatchDetail, DiscoveryBatchSummary } from '$lib/types';

	const batchesQuery = getDiscoveryBatchesQuery();
	const batches = $derived(batchesQuery.data?.batches ?? []);

	const expanded = new SvelteSet<string>();
	let details = $state<Record<string, DiscoveryBatchDetail>>({});
	let confirming = $state<DiscoveryBatchSummary | null>(null);
	let removing = $state(false);
	let confirmDialog: HTMLDialogElement | undefined = $state();

	async function toggleExpand(batch: DiscoveryBatchSummary) {
		if (expanded.has(batch.id)) {
			expanded.delete(batch.id);
			return;
		}
		expanded.add(batch.id);
		if (!details[batch.id]) {
			try {
				details = {
					...details,
					[batch.id]: await api.global.get<DiscoveryBatchDetail>(API.discoverBatch(batch.id))
				};
			} catch {
				expanded.delete(batch.id);
			}
		}
	}

	$effect(() => {
		if (!confirmDialog) return;
		if (confirming) {
			confirmDialog.showModal();
		} else if (confirmDialog.open) {
			confirmDialog.close();
		}
	});

	async function confirmRemove(removeAlbums: boolean) {
		if (!confirming || removing) return;
		removing = true;
		try {
			await removeDiscoveryBatch(confirming.id, removeAlbums);
			expanded.delete(confirming.id);
			confirming = null;
		} finally {
			removing = false;
		}
	}

	function statusLabel(status: string | null, inLibrary: boolean): string {
		if (inLibrary) return 'Imported';
		switch (status) {
			case 'awaiting_approval':
				return 'Awaiting approval';
			case 'pending':
			case 'searching':
			case 'downloading':
			case 'processing':
				return 'In progress';
			case 'failed':
				return 'Failed';
			case 'cancelled':
				return 'Cancelled';
			default:
				return status ?? '—';
		}
	}
</script>

{#if batches.length > 0}
	<section class="mt-8">
		<div class="mb-3 flex items-center gap-2">
			<Sparkles class="h-4 w-4 text-primary" />
			<h2 class="text-lg font-bold">Discovery Batches</h2>
			<span class="text-xs text-base-content/40">
				Sections you grabbed in one go - removable in one go, too
			</span>
		</div>

		<div class="space-y-2">
			{#each batches as batch (batch.id)}
				<div class="rounded-xl border border-base-content/10 bg-base-200/40">
					<div class="flex w-full items-center gap-3 px-4 py-3">
						<button
							class="flex min-w-0 flex-1 items-center gap-3 text-left"
							onclick={() => toggleExpand(batch)}
							aria-expanded={expanded.has(batch.id)}
						>
							<div class="min-w-0 flex-1">
								<p class="truncate font-semibold">{batch.name}</p>
								<p class="text-xs text-base-content/50">
									{new Date(batch.created_at).toLocaleDateString()} ·
									{batch.imported_count}/{batch.item_count} imported
									{#if batch.pending_count}
										· {batch.pending_count} pending
									{/if}
								</p>
							</div>
							<ChevronDown
								class="h-4 w-4 shrink-0 text-base-content/40 transition-transform {expanded.has(
									batch.id
								)
									? 'rotate-180'
									: ''}"
							/>
						</button>
						<button
							class="btn btn-ghost btn-sm gap-1 text-error/80 shrink-0"
							onclick={() => (confirming = batch)}
						>
							<Trash2 class="h-4 w-4" />
							Remove
						</button>
					</div>

					{#if expanded.has(batch.id)}
						<div
							transition:slide={{ duration: 200 }}
							class="border-t border-base-content/5 px-4 py-2"
						>
							{#if details[batch.id]}
								<ul class="divide-y divide-base-content/5">
									{#each details[batch.id].items as item (item.release_group_mbid)}
										{@const href = albumHrefOrNull(item.release_group_mbid)}
										<li class="flex items-center gap-3 py-1.5 text-sm">
											<svelte:element
												this={href ? 'a' : 'span'}
												href={href ?? undefined}
												class="min-w-0 flex-1 truncate {href ? 'hover:text-primary' : ''}"
											>
												{item.album_name}
												<span class="text-base-content/50">— {item.artist_name}</span>
											</svelte:element>
											{#if item.outcome !== 'requested'}
												<span class="badge badge-ghost badge-sm shrink-0">Already yours</span>
											{:else}
												<span
													class="badge badge-sm shrink-0 {item.in_library
														? 'badge-success'
														: 'badge-ghost'}"
												>
													{statusLabel(item.request_status, item.in_library)}
												</span>
											{/if}
										</li>
									{/each}
								</ul>
							{:else}
								<div class="flex justify-center py-4">
									<LoaderCircle class="h-5 w-5 animate-spin text-base-content/40" />
								</div>
							{/if}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	</section>
{/if}

<dialog bind:this={confirmDialog} class="modal" onclose={() => (confirming = null)}>
	<div class="modal-box max-w-md">
		{#if confirming}
			<h3 class="text-lg font-bold">Remove “{confirming.name}”?</h3>
			<p class="mt-2 text-sm text-base-content/70">
				Removes the {confirming.imported_count} downloaded album{confirming.imported_count === 1
					? ''
					: 's'} to the <strong>recycle bin</strong> (restorable), cancels anything still pending, and
				never touches albums you already had before the batch.
			</p>
			<div class="modal-action flex-wrap">
				<button class="btn btn-ghost btn-sm" onclick={() => (confirming = null)}>Cancel</button>
				<button
					class="btn btn-outline btn-sm"
					disabled={removing}
					onclick={() => confirmRemove(false)}
				>
					Keep albums, forget batch
				</button>
				<button
					class="btn btn-error btn-sm gap-2"
					disabled={removing}
					onclick={() => confirmRemove(true)}
				>
					{#if removing}
						<LoaderCircle class="h-4 w-4 animate-spin" />
					{:else}
						<Trash2 class="h-4 w-4" />
					{/if}
					Remove batch & albums
				</button>
			</div>
		{/if}
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
