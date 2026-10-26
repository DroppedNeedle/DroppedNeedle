<script lang="ts">
	import { Check, ChevronDown, RefreshCw, RotateCcw, Search, Trash2 } from 'lucide-svelte';
	import { SvelteMap } from 'svelte/reactivity';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import DropImportMatchModal from '$lib/components/import/DropImportMatchModal.svelte';
	import {
		discardHeldMutation,
		importHeldMutation,
		recheckHeldMutation
	} from '$lib/queries/downloads/HeldQueries.svelte';
	import { retryDownload } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { HeldEntry, HeldFileAction } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';

	import HeldFileRow from './HeldFileRow.svelte';
	import { keptUntil, nextTry } from './heldExpiry';

	interface Props {
		entry: HeldEntry;
		/** The upload's name, where the card sits inside its job. */
		name?: string;
		/** Drop the cover and title where the job above already names the upload. */
		compact?: boolean;
		/** A download that grabbed the wrong edition: its task, which can hunt again. */
		retryTaskId?: string | null;
	}

	let { entry, name, compact = false, retryTaskId = null }: Props = $props();

	const importMut = importHeldMutation();
	const recheckMut = recheckHeldMutation();
	const discardMut = discardHeldMutation();
	const retryTask = retryDownload();
	const busy = $derived(
		importMut.isPending ||
			recheckMut.isPending ||
			discardMut.isPending ||
			retryTask.isPending ||
			entry.state !== 'held'
	);

	const files = $derived(
		[...entry.files].sort(
			(a, b) =>
				(a.disc_number ?? 1) - (b.disc_number ?? 1) ||
				(a.track_number ?? 0) - (b.track_number ?? 0) ||
				a.id - b.id
		)
	);
	const single = $derived(files.length === 1);

	// An upload's files keep the folder they came in; its first part names it.
	const folder = $derived(files[0]?.original_name.split('/')[0] ?? null);
	const title = $derived(entry.album_title ?? name ?? folder ?? 'Held files');
	const subtitle = $derived(
		[
			entry.artist_name,
			`${files.length} ${files.length === 1 ? 'file' : 'files'}`,
			entry.origin === 'upload' ? 'Upload' : 'Download'
		]
			.filter(Boolean)
			.join(' · ')
	);

	// Each distinct reason once, with how many files it holds when the entry has several.
	const reasons = $derived.by(() => {
		const counts = new SvelteMap<string, number>();
		for (const file of files) counts.set(file.reason_text, (counts.get(file.reason_text) ?? 0) + 1);
		return [...counts].map(([text, count]) => ({ text, count }));
	});

	function idsOffering(action: HeldFileAction): number[] {
		return files.filter((file) => file.actions.includes(action)).map((file) => file.id);
	}
	const importIds = $derived(idsOffering('import'));
	const retryIds = $derived(idsOffering('retry'));
	const recheckIds = $derived(idsOffering('recheck'));
	const canMatch = $derived(authStore.isTrusted);

	const expiry = $derived(keptUntil(entry.expires_at));
	const retrying = $derived(
		entry.files.some((file) => file.actions.includes('retry'))
			? entry.state === 'placing'
				? 'Retrying now'
				: nextTry(entry.next_retry_at)
			: null
	);

	const forFiles = (fileIds: number[]) => ({
		entryId: entry.id,
		fileIds,
		releaseGroupMbid: entry.release_group_mbid
	});

	let filesOpen = $state(false);
	let discardOpen = $state(false);
	let matching = $state(false);
</script>

{#snippet retryDownloadButton(size: 'xs' | 'sm')}
	<Button
		variant="ghost"
		{size}
		icon={RefreshCw}
		loading={retryTask.isPending}
		onclick={() => retryTaskId && retryTask.mutate(retryTaskId)}
		disabled={busy}
	>
		{retryTask.isPending ? 'Retrying…' : 'Retry download'}
	</Button>
{/snippet}

<Card as="article" variant="bordered" padding="md" class="border-warning/25">
	<div class="flex items-start gap-3 sm:gap-4">
		{#if !compact}
			<div
				class="relative size-14 shrink-0 overflow-hidden rounded-xl ring-1 ring-base-content/10 sm:size-16"
			>
				{#if entry.release_group_mbid}
					<AlbumImage
						mbid={entry.release_group_mbid}
						alt={title}
						size="sm"
						rounded="xl"
						className="h-full w-full"
					/>
				{:else}
					<CoverPlaceholder name={title} />
				{/if}
			</div>
		{/if}

		<div class="min-w-0 flex-1">
			{#if !compact}
				<h3 class="truncate text-base font-bold">
					{#if entry.release_group_mbid}
						<a
							href={albumHref(entry.release_group_mbid)}
							class="transition-colors hover:text-primary motion-reduce:transition-none"
						>
							{title}
						</a>
					{:else}
						{title}
					{/if}
				</h3>
			{/if}
			<p class="text-xs text-base-content/60">
				{subtitle} · <span class="whitespace-nowrap">{expiry}</span>
			</p>
			{#if retrying}
				<p class="mt-0.5 flex items-center gap-1.5 text-xs text-base-content/60">
					<RefreshCw
						class="size-3 shrink-0 {entry.state === 'placing' ? 'animate-spin' : ''}"
						aria-hidden="true"
					/>
					<span>{retrying}</span>
				</p>
			{/if}
			{#if single && files[0]}
				<div class="mt-3 border-t border-base-content/8 pt-3">
					<HeldFileRow {entry} file={files[0]}>
						{#if retryTaskId}
							{@render retryDownloadButton('xs')}
						{/if}
						{#if canMatch}
							<Button
								variant="ghost"
								size="xs"
								icon={Search}
								onclick={() => (matching = true)}
								disabled={busy}
							>
								Match…
							</Button>
						{/if}
					</HeldFileRow>
				</div>
			{:else}
				<ul class="mt-2 space-y-0.5 text-sm text-base-content/75">
					{#each reasons as reason (reason.text)}
						<li>
							{reason.text}{#if reasons.length > 1}<span class="text-base-content/45"
									>{` (${reason.count})`}</span
								>{/if}
						</li>
					{/each}
				</ul>

				<div class="mt-3 flex flex-wrap items-center gap-2">
					{#if importIds.length > 0}
						<Button
							variant="primary"
							size="sm"
							icon={Check}
							onclick={() => importMut.mutate(forFiles(importIds))}
							disabled={busy}
						>
							{importIds.length === files.length ? 'Import all' : `Import ${importIds.length}`}
						</Button>
					{/if}
					{#if retryIds.length > 0}
						<Button
							variant="primary"
							size="sm"
							icon={RotateCcw}
							onclick={() => importMut.mutate(forFiles(retryIds))}
							disabled={busy}
						>
							Retry
						</Button>
					{/if}
					{#if retryTaskId}
						{@render retryDownloadButton('sm')}
					{/if}
					{#if canMatch}
						<Button
							variant={importIds.length === 0 && retryIds.length === 0 ? 'primary' : 'ghost'}
							size="sm"
							icon={Search}
							onclick={() => (matching = true)}
							disabled={busy}
						>
							Match…
						</Button>
					{/if}
					{#if recheckIds.length > 0}
						<Button
							variant="ghost"
							size="sm"
							icon={RotateCcw}
							loading={recheckMut.isPending}
							onclick={() => recheckMut.mutate(forFiles(recheckIds))}
							disabled={busy}
						>
							Re-check
						</Button>
					{/if}
					<Button
						variant="ghost"
						size="sm"
						icon={Trash2}
						onclick={() => (discardOpen = true)}
						disabled={busy}
					>
						Discard all
					</Button>
				</div>

				<div class="mt-3 border-t border-base-content/8 pt-3">
					<button
						type="button"
						class="flex w-full items-center justify-between gap-3 text-left text-xs font-semibold text-base-content/60 hover:text-base-content"
						onclick={() => (filesOpen = !filesOpen)}
						aria-expanded={filesOpen}
					>
						<span>{filesOpen ? 'Hide files' : `Show ${files.length} files`}</span>
						<ChevronDown
							class="size-4 transition-transform motion-reduce:transition-none {filesOpen
								? 'rotate-180'
								: ''}"
							aria-hidden="true"
						/>
					</button>
					{#if filesOpen}
						<ul class="mt-3 divide-y divide-base-content/8">
							{#each files as file (file.id)}
								<li class="py-3 first:pt-0 last:pb-0">
									<HeldFileRow {entry} {file} />
								</li>
							{/each}
						</ul>
					{/if}
				</div>
			{/if}
		</div>
	</div>
</Card>

<ConfirmDialog
	bind:open={discardOpen}
	title="Discard {files.length} files?"
	message="They are deleted. Nothing in your library changes."
	confirmLabel="Discard"
	tone="danger"
	pending={discardMut.isPending}
	onconfirm={() => discardMut.mutateAsync(forFiles(files.map((file) => file.id)))}
/>

{#if matching}
	<DropImportMatchModal
		entryId={entry.id}
		{name}
		search={entry.album_title ?? name ?? folder ?? ''}
		fileCount={files.length}
		releaseGroupMbid={entry.release_group_mbid}
		onclose={() => (matching = false)}
	/>
{/if}
