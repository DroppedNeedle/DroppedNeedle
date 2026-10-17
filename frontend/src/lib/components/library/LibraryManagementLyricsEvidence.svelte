<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { FileText } from 'lucide-svelte';

	import type { LibraryManagementPlanItem } from '$lib/queries/library-management/types';
	import Card from '$lib/ui/Card.svelte';
	import { managementFieldDiffs, managementLyricsProjection } from './LibraryManagementDisplay';

	interface Props {
		item: LibraryManagementPlanItem;
		workState?: string | null;
		operationState?: string | null;
	}

	let { item, workState = null, operationState = null }: Props = $props();
	const projection = $derived(managementLyricsProjection(item));
	const fieldDiffs = $derived(managementFieldDiffs(item));
	const plainDiff = $derived(fieldDiffs.find((value) => value.name === 'lyrics_plain') ?? null);
	const syncedDiff = $derived(fieldDiffs.find((value) => value.name === 'lyrics_synced') ?? null);

	function statusLabel(): string {
		if (!projection) return 'Unavailable';
		return {
			available: 'Exact match pinned',
			mismatch: 'Signature mismatch',
			not_found: 'No exact match',
			deferred: 'Provider deferred'
		}[projection.status];
	}

	function writeLabel(selected: boolean, available: boolean, operation: string | null): string {
		if (operation === 'unchanged') return 'Already matches';
		if (operation === 'preserve') return 'Preserved by profile';
		if (operation !== null && ['set', 'clear', 'merge'].includes(operation)) {
			if (workState === 'succeeded' || workState === 'completed') return 'Written';
			if (['failed', 'skipped', 'not_scheduled'].includes(workState ?? '')) return 'Not written';
			if (
				operationState !== null &&
				['failed', 'cancelled', 'discarded', 'stopped', 'succeeded'].includes(operationState)
			) {
				return 'Not written';
			}
			if (workState === 'running') return 'Writing';
			return 'Will be written';
		}
		if (selected && projection?.preserveExisting && available) {
			return 'Existing lyrics preserved';
		}
		return available ? 'Available · no write planned' : 'Unavailable · no write planned';
	}

	function synchronizedWriteLabel(): string {
		if (projection?.syncedSelected && !projection.syncedSupported && projection.plainSelected) {
			return 'Plain fallback for this format';
		}
		return writeLabel(
			projection?.syncedSelected ?? false,
			projection?.syncedAvailable ?? false,
			syncedDiff?.operation ?? null
		);
	}
</script>

{#if projection}
	<section class="rounded-xl border border-library-manage/20 bg-library-manage/5 p-3">
		<div class="flex flex-wrap items-start justify-between gap-2">
			<div class="flex items-start gap-2">
				<FileText class="mt-0.5 h-4 w-4 shrink-0 text-library-manage" />
				<div>
					<h4 class="management-inspector-section-title">Lyrics evidence</h4>
					<p class="mt-1 text-xs text-base-content/55">
						{#if projection.status === 'available'}LRCLIB matched the exact title, artist, album,
							and duration signature.{:else}{projection.reason ??
								'No exact lyrics result was accepted.'}{/if}
					</p>
				</div>
			</div>
			<span
				class="badge badge-sm {projection.status === 'available'
					? 'badge-success'
					: 'badge-warning'}">{statusLabel()}</span
			>
		</div>
		<div class="mt-3 grid gap-2 sm:grid-cols-2">
			<Card variant="bordered" padding="sm">
				<small class="block text-base-content/45">Plain lyrics</small>
				<strong class="text-sm"
					>{writeLabel(
						projection.plainSelected,
						projection.plainAvailable,
						plainDiff?.operation ?? null
					)}</strong
				>
			</Card>
			<Card variant="bordered" padding="sm">
				<small class="block text-base-content/45">Synchronized lyrics</small>
				<strong class="text-sm">{synchronizedWriteLabel()}</strong>
			</Card>
		</div>
		{#if projection.status !== 'available'}
			<p class="mt-3 text-xs font-medium text-base-content/65">
				No lyrics tag change is planned. Any lyrics already in this file remain untouched.
			</p>
		{/if}
	</section>
{/if}
