<script lang="ts">
	import { RefreshCw } from 'lucide-svelte';

	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import { getLidarrImportCandidatesQuery } from '$lib/queries/lidarr-import/LidarrImportQueries.svelte';
	import { importFromLidarrMutation } from '$lib/queries/lidarr-import/LidarrImportMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { LidarrImportResult } from '$lib/queries/lidarr-import/types';

	let { open = $bindable(false), onImported }: { open?: boolean; onImported?: () => void } =
		$props();

	let dialogEl: HTMLDialogElement | undefined = $state();
	let selected = $state<string[]>([]);
	let result = $state<LidarrImportResult | null>(null);
	let importError = $state<string | null>(null);
	let seeded = false;

	const candidatesQuery = getLidarrImportCandidatesQuery(() => open);
	const candidates = $derived(candidatesQuery.data?.artists ?? []);
	const selectable = $derived(candidates.filter((c) => !c.already_following));
	const allSelected = $derived(selectable.length > 0 && selected.length === selectable.length);
	const importMutation = importFromLidarrMutation();

	let wasOpen = false;
	$effect(() => {
		if (open && !wasOpen) {
			selected = [];
			result = null;
			importError = null;
			seeded = false;
		}
		wasOpen = open;
		if (open) dialogEl?.showModal();
		else dialogEl?.close();
	});

	// D7: pre-check every not-yet-followed row once the candidates load.
	$effect(() => {
		if (open && !seeded && candidates.length > 0) {
			selected = selectable.map((c) => c.mbid);
			seeded = true;
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
			seeded = true; // don't re-seed the (now emptied) selection after the result renders
			onImported?.();
		} catch (e: unknown) {
			importError = (e as { message?: string })?.message ?? 'Could not import from Lidarr';
		}
	}

	function close() {
		open = false;
	}
</script>

<dialog bind:this={dialogEl} class="modal" onclose={close}>
	<div class="modal-box max-w-2xl">
		<h3 class="text-lg font-bold">Import from Lidarr</h3>
		<p class="mt-0.5 text-sm text-base-content/60">
			One-time import of your monitored artists as follows. Lidarr stays independent.
		</p>

		{#if !result}
			<div class="mt-4 flex items-center justify-between">
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

		<div class="mt-3 max-h-80 space-y-1.5 overflow-y-auto">
			{#if candidatesQuery.isPending}
				{#each Array(4) as _, i (`lidarr-skel-${i}`)}
					<div class="flex animate-pulse items-center gap-3 rounded-box bg-base-300/40 p-2.5">
						<div class="size-5 rounded bg-base-300"></div>
						<div class="size-10 rounded-lg bg-base-300"></div>
						<div class="h-3.5 w-40 rounded bg-base-300"></div>
					</div>
				{/each}
			{:else if candidatesQuery.isError}
				<div class="alert alert-error py-2 text-sm">
					Couldn't load your monitored artists from Lidarr.
				</div>
			{:else if candidates.length === 0}
				<div class="py-10 text-center text-sm text-base-content/50">
					No monitored artists found in Lidarr.
				</div>
			{:else}
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
			{/if}
		</div>

		{#if result}
			<div class="alert alert-success mt-4 flex-col items-start py-2.5 text-sm">
				<span class="font-medium">
					{result.imported} imported · {result.already_following} already following · {result.skipped_invalid}
					skipped
				</span>
				{#if result.auto_download_enabled > 0}
					<span class="text-success-content/80">
						{#if authStore.isAdmin}
							Auto-download enabled for {result.auto_download_enabled} artists.
						{:else}
							Auto-download for {result.auto_download_enabled} artists is pending admin approval.
						{/if}
					</span>
				{/if}
			</div>
		{/if}
		{#if importError}
			<div class="alert alert-error mt-3 py-2 text-sm">{importError}</div>
		{/if}

		<div class="modal-action">
			{#if result}
				<button class="btn btn-primary btn-sm" onclick={close}>Done</button>
			{:else}
				<button class="btn btn-ghost btn-sm" onclick={close}>Cancel</button>
				<button
					class="btn btn-primary btn-sm"
					disabled={selected.length === 0 || importMutation.isPending}
					onclick={() => void runImport()}
				>
					{#if importMutation.isPending}
						<span class="loading loading-spinner loading-xs"></span>
					{:else}
						<RefreshCw class="size-4" aria-hidden="true" />
					{/if}
					Import{selected.length > 0 ? ` ${selected.length}` : ''}
				</button>
			{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
