<script lang="ts">
	import { ArrowDownToLine, CircleCheck, Lock, RefreshCw } from 'lucide-svelte';

	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import { getLidarrImportCandidatesQuery } from '$lib/queries/lidarr-import/LidarrImportQueries.svelte';
	import { importFromLidarrMutation } from '$lib/queries/lidarr-import/LidarrImportMutations.svelte';
	import type { LidarrImportResult } from '$lib/queries/lidarr-import/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		unlocked: boolean;
	}

	let { unlocked }: Props = $props();

	let selected = $state<string[]>([]);
	let result = $state<LidarrImportResult | null>(null);
	let importError = $state<string | null>(null);
	let seededFor = $state<string | null>(null);

	const candidatesQuery = getLidarrImportCandidatesQuery(() => unlocked);
	const candidates = $derived(candidatesQuery.data?.artists ?? []);
	const total = $derived(candidatesQuery.data?.total ?? 0);
	const selectable = $derived(candidates.filter((c) => !c.already_following));
	const followedCount = $derived(candidates.filter((c) => c.already_following).length);
	const newCount = $derived(selectable.length);
	const allSelected = $derived(selectable.length > 0 && selected.length === selectable.length);
	const importMutation = importFromLidarrMutation();

	// Seed key: the candidate list identity. Reset the pre-check when the list
	// identity changes (fresh fetch, connection change), but never re-seed once
	// an import result is showing.
	const listKey = $derived(
		candidates.map((c) => `${c.mbid}:${c.already_following ? '1' : '0'}`).join(',')
	);

	$effect(() => {
		if (!unlocked) {
			selected = [];
			seededFor = null;
			return;
		}
		if (result) return;
		if (candidates.length > 0 && seededFor !== listKey) {
			selected = selectable.map((c) => c.mbid);
			seededFor = listKey;
		}
	});

	function toggle(mbid: string) {
		selected = selected.includes(mbid) ? selected.filter((m) => m !== mbid) : [...selected, mbid];
	}

	function toggleAll() {
		selected = allSelected ? [] : selectable.map((c) => c.mbid);
	}

	async function runImport() {
		importError = null;
		result = null;
		try {
			result = await importMutation.mutateAsync(selected);
			selected = [];
		} catch (e: unknown) {
			importError = (e as { message?: string })?.message ?? 'Could not import from Lidarr';
		}
	}
</script>

<Card variant="bordered" padding="none">
	<div>
		<div class="flex items-center gap-3 p-4">
			<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
				<ArrowDownToLine class="size-6 text-accent" aria-hidden="true" />
			</div>
			<div>
				<h3 class="text-lg font-bold">Artist sync</h3>
				<p class="text-sm text-base-content/60">Sync your monitored artists into your follows.</p>
			</div>
		</div>

		<div class="space-y-4 border-t border-base-300 p-5">
			{#if !unlocked}
				<div class="flex items-center gap-3 rounded-box bg-base-300/30 p-4 opacity-60">
					<Lock class="size-5 shrink-0 text-base-content/50" aria-hidden="true" />
					<p class="text-sm text-base-content/60">Connect Lidarr above to unlock artist sync.</p>
				</div>
			{:else if candidatesQuery.isPending}
				<div class="space-y-1.5" aria-label="Loading monitored artists">
					{#each Array(4) as _, i (`lidarr-sync-skel-${i}`)}
						<div class="flex animate-pulse items-center gap-3 rounded-box bg-base-300/40 p-2.5">
							<div class="size-5 rounded bg-base-300"></div>
							<div class="size-10 rounded-lg bg-base-300"></div>
							<div class="h-3.5 w-40 rounded bg-base-300"></div>
						</div>
					{/each}
				</div>
			{:else if candidatesQuery.isError}
				<div class="alert alert-error py-2 text-sm">
					Couldn't reach Lidarr - check the connection above.
				</div>
				<Button size="sm" onclick={() => void candidatesQuery.refetch()}>Retry</Button>
			{:else if total === 0}
				<p class="py-6 text-center text-sm text-base-content/50">No monitored artists in Lidarr.</p>
				<Button size="sm" onclick={() => void candidatesQuery.refetch()}>Check again</Button>
			{:else if newCount > 0}
				<div class="alert py-2 text-sm">
					{followedCount} of {total} in your follows · {newCount} new
				</div>

				{#if !result}
					<div class="flex items-center justify-between">
						<label class="flex cursor-pointer items-center gap-2 text-sm font-medium">
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={allSelected}
								disabled={selectable.length === 0}
								onchange={toggleAll}
							/>
							Select all
						</label>
						<span class="text-sm text-base-content/60">
							{selected.length} of {selectable.length} selected
						</span>
					</div>
				{/if}

				<div class="max-h-80 space-y-1.5 overflow-y-auto">
					{#each candidates as candidate (candidate.mbid)}
						<label
							class="flex items-center gap-3 rounded-box p-2.5 transition-colors {candidate.already_following
								? 'opacity-50'
								: 'cursor-pointer bg-base-300/30 hover:bg-base-300/50'}"
						>
							<input
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={selected.includes(candidate.mbid)}
								disabled={candidate.already_following || !!result}
								onchange={() => toggle(candidate.mbid)}
							/>
							<div class="size-10 shrink-0 overflow-hidden rounded-lg">
								<ArtistImage
									mbid={candidate.mbid}
									alt={candidate.name}
									className="h-full w-full object-cover"
								/>
							</div>
							<span class="min-w-0 flex-1 truncate text-sm font-medium">{candidate.name}</span>
							{#if candidate.would_auto_download}
								<span class="badge badge-ghost badge-sm shrink-0">Auto-download</span>
							{/if}
							{#if candidate.already_following}
								<span class="badge badge-ghost badge-sm shrink-0">Following</span>
							{/if}
						</label>
					{/each}
				</div>

				{#if result}
					<div class="alert alert-success flex-col items-start py-2.5 text-sm">
						<span class="font-medium">
							{result.imported} imported · {result.already_following} already following · {result.skipped_invalid}
							skipped
						</span>
						{#if result.auto_download_enabled > 0}
							<span class="text-success-content/80">
								Auto-download enabled for {result.auto_download_enabled} artists.
							</span>
						{/if}
					</div>
				{/if}
				{#if importError}
					<div class="alert alert-error py-2 text-sm">{importError}</div>
				{/if}

				<div class="flex items-center justify-end gap-3">
					{#if result}
						<Button
							size="sm"
							onclick={() => {
								result = null;
								importError = null;
								seededFor = null;
							}}
						>
							Select remaining
						</Button>
					{/if}
					<Button
						variant="primary"
						size="sm"
						icon={RefreshCw}
						loading={importMutation.isPending}
						disabled={selected.length === 0}
						onclick={() => void runImport()}
					>
						Import{selected.length > 0 ? ` ${selected.length}` : ''}
					</Button>
				</div>
			{:else}
				<div class="alert alert-success py-2 text-sm">
					<CircleCheck class="size-4 shrink-0" aria-hidden="true" />
					All {total} monitored artists are in your follows.
				</div>
				<Button size="sm" onclick={() => void candidatesQuery.refetch()}>Check again</Button>

				{#if result}
					<div class="alert alert-success flex-col items-start py-2.5 text-sm">
						<span class="font-medium">
							{result.imported} imported · {result.already_following} already following · {result.skipped_invalid}
							skipped
						</span>
						{#if result.auto_download_enabled > 0}
							<span class="text-success-content/80">
								Auto-download enabled for {result.auto_download_enabled} artists.
							</span>
						{/if}
					</div>
				{/if}
				{#if importError}
					<div class="alert alert-error py-2 text-sm">{importError}</div>
				{/if}
			{/if}
		</div>
	</div>
</Card>
