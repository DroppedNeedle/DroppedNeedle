<!--
	The recycle bin for admins: each removal or replaced copy still in the bin,
	with Put back where it can be, and "Empty recycle bin", which deletes what
	the bin held when its confirm was read, in the background.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import { Trash2, Undo2 } from 'lucide-svelte';
	import { failureSentence } from '$lib/components/library/edition/editionCopy';
	import { notBackLine, putBackToast } from '$lib/components/library/recycledRemovalsCopy';
	import {
		invalidateAlbumRemovals,
		putBackRemoval
	} from '$lib/queries/library/RemovalQueries.svelte';
	import {
		emptyRecycleBin,
		getRecycleBinQuery,
		getRecycleBinStatusQuery,
		getRecycleBinSummaryQuery,
		invalidateRecycleBin,
		type BinRemoval
	} from '$lib/queries/library/RecycleBinQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { albumHref } from '$lib/utils/entityRoutes';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import PageShell from '$lib/ui/PageShell.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import {
		binLine,
		binNotes,
		emptiedToast,
		emptyQuestion,
		lastEmptiedLine,
		removedLine,
		retentionLine,
		stayingLine
	} from './recycleBinCopy';

	const bin = getRecycleBinQuery();
	const status = getRecycleBinStatusQuery();
	const empty = emptyRecycleBin();
	const putBack = putBackRemoval();

	let confirmOpen = $state(false);
	const summary = getRecycleBinSummaryQuery(() => confirmOpen);
	let notes = $state<Record<string, string>>({});
	let notBack = $state<string | null>(null);
	let wasEmptying = false;
	const now = Date.now();

	const emptying = $derived(status.data?.state === 'emptying');
	const firstPage = $derived(bin.data?.pages[0]);
	const removals = $derived(bin.data?.pages.flatMap((page) => page.items) ?? []);
	const somethingToDelete = $derived(
		!!summary.data && (summary.data.files > 0 || summary.data.older_folders > 0)
	);

	// Every status read counts, so a run that finished before its first poll
	// still refreshes the page.
	$effect(() => {
		void status.dataUpdatedAt;
		const state = status.data?.state;
		if (wasEmptying && state && state !== 'emptying') {
			void invalidateRecycleBin();
			void invalidateAlbumRemovals();
			const last = status.data?.last_emptied;
			if (state === 'idle' && last) {
				toastStore.show({ message: emptiedToast(last), type: 'success' });
			}
		}
		wasEmptying = state === 'emptying';
	});

	function openConfirm(): void {
		confirmOpen = true;
	}

	async function runEmpty(): Promise<void> {
		const asOf = summary.data?.as_of;
		if (asOf === undefined) return;
		wasEmptying = true;
		try {
			const answer = await empty.mutateAsync(asOf);
			if (answer.joined) toastStore.show({ message: 'Already emptying.', type: 'info' });
		} catch (error) {
			wasEmptying = false;
			throw error;
		}
	}

	async function runPutBack(removal: BinRemoval): Promise<void> {
		if (!removal.album) return;
		notes = { ...notes, [removal.id]: '' };
		notBack = null;
		try {
			const answer = await putBack.mutateAsync({
				albumId: removal.album.id,
				removalId: removal.id
			});
			toastStore.show({
				message: putBackToast(removal.files - answer.missing.length),
				type: 'success'
			});
			notBack = notBackLine(answer.missing);
		} catch (error) {
			notes = { ...notes, [removal.id]: failureSentence(error, "The files couldn't be put back.") };
			void invalidateRecycleBin();
		}
	}
</script>

<PageShell title="Recycle Bin" subtitle={retentionLine(firstPage?.retention_days)} width="narrow">
	{#snippet actions()}
		<Button
			variant="danger"
			icon={Trash2}
			disabled={emptying}
			loading={emptying}
			onclick={openConfirm}>{emptying ? 'Emptying...' : 'Empty recycle bin'}</Button
		>
	{/snippet}

	<div class="flex flex-col gap-3">
		{#if status.data?.state === 'stopped'}
			<p class="text-sm text-warning" role="status">Emptying stopped. Try again.</p>
		{/if}
		{#if status.data?.last_emptied}
			<p class="text-sm text-base-content/70">{lastEmptiedLine(status.data.last_emptied)}</p>
		{/if}
		{#if firstPage && !firstPage.bins_known}
			<p class="text-sm text-warning" role="status">Can't check the recycle bin right now.</p>
		{/if}
		{#if notBack}<p class="text-sm text-warning" role="status">{notBack}</p>{/if}

		<QueryBoundary query={bin} what="the recycle bin" isEmpty={() => removals.length === 0}>
			{#snippet empty()}
				<Card variant="bordered" padding="lg">
					<p class="text-center text-base-content/70">The recycle bin is empty.</p>
				</Card>
			{/snippet}
			{#snippet children(data)}
				<ul class="flex flex-col gap-3" aria-label="In the recycle bin">
					{#each data.pages.flatMap((page) => page.items) as removal (removal.id)}
						<li>
							<Card variant="bordered" padding="md">
								<div class="flex flex-wrap items-center gap-3">
									<div class="relative size-12 shrink-0 overflow-hidden rounded-lg">
										<AlbumImage
											albumId={removal.album?.id}
											mbid={removal.album?.release_group_mbid ?? ''}
											available={removal.album?.cover_available ?? false}
											alt={removal.album?.title ?? 'Album'}
											size="full"
											requestSize={250}
											rounded="none"
											className="h-full w-full"
										/>
									</div>
									<div class="min-w-0 flex-1">
										<p class="truncate font-semibold">
											{#if removal.album?.has_page}
												<a class="link-hover" href={albumHref(removal.album.id)}
													>{removal.album.title}</a
												>
											{:else}
												{removal.album?.title ?? 'Unknown album'}
											{/if}
										</p>
										{#if removal.album?.artist}
											<p class="truncate text-sm text-base-content/70">{removal.album.artist}</p>
										{/if}
										<p class="text-sm">{binLine(removal, now)}</p>
										<p class="text-xs text-base-content/60">{removedLine(removal)}</p>
										{#each binNotes(removal) as note (note)}
											<p class="text-xs text-warning">{note}</p>
										{/each}
										{#if removal.held > 0}
											<p class="text-xs text-base-content/60">
												Held: settle it in <a class="link" href={resolve('/library/management')}
													>Library Management</a
												>
											</p>
										{/if}
									</div>
									{#if removal.can_put_back}
										<AsyncButton
											variant="outline"
											size="sm"
											icon={Undo2}
											disabled={emptying}
											pendingLabel="Putting back..."
											onclick={() => runPutBack(removal)}>Put back</AsyncButton
										>
									{/if}
									{#if notes[removal.id]}
										<p class="basis-full text-sm text-error" role="alert">{notes[removal.id]}</p>
									{/if}
								</div>
							</Card>
						</li>
					{/each}
				</ul>
				{#if bin.hasNextPage}
					<div class="flex justify-center">
						<Button
							variant="ghost"
							loading={bin.isFetchingNextPage}
							onclick={() => bin.fetchNextPage()}>Show more</Button
						>
					</div>
				{/if}
			{/snippet}
		</QueryBoundary>
	</div>
</PageShell>

<ConfirmDialog
	bind:open={confirmOpen}
	title="Empty the recycle bin?"
	confirmLabel="Empty"
	tone="danger"
	pending={summary.isFetching}
	canConfirm={!summary.data || somethingToDelete}
	onconfirm={runEmpty}
>
	{#if summary.data}
		{@const question = emptyQuestion(summary.data)}
		<p>{question ?? 'Nothing to delete right now.'}</p>
		{#if stayingLine(summary.data)}
			<p class="mt-2 text-sm text-base-content/70">{stayingLine(summary.data)}</p>
		{/if}
		{#if question}
			<p class="mt-2 text-sm text-base-content/70">Files changed in the bin are kept.</p>
		{/if}
	{/if}
</ConfirmDialog>
