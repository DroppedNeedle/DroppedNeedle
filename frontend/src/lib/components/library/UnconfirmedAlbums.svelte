<!--
	One Check albums list: albums whose edition is a best guess, that
	nothing fits, or that someone marked "Looks wrong". Each row shows the
	files and, for a guess, what "That's right" confirms. Compare opens the
	album's edition sheet, and Done there comes back here at the next album.
-->
<script lang="ts" module>
	export type WaitingView = 'unconfirmed' | 'unmatched' | 'looks_wrong';
</script>

<script lang="ts">
	import { tick } from 'svelte';
	import { CircleAlert, CircleHelp, Flag } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import { coverPath } from '$lib/api/covers';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		createClearLooksWrongMutation,
		createConfirmAlbumEditionMutation,
		createUndoAlbumEditionMutation,
		getWaitingAlbumsQuery,
		type WaitingAlbum
	} from '$lib/queries/albums/EditionQueries.svelte';
	import {
		MATCH_FILES,
		NEEDS_MATCHING,
		count,
		failureSentence,
		fitLine
	} from './edition/editionCopy';

	interface Props {
		view: WaitingView;
		/** The album to bring into view first: the next one after an album compared. */
		focus?: string | null;
	}

	let { view, focus = null }: Props = $props();

	const PAGE = 50;
	const userId = () => authStore.user?.id;
	const query = getWaitingAlbumsQuery(userId, () => view, PAGE);
	const confirm = createConfirmAlbumEditionMutation();
	const undo = createUndoAlbumEditionMutation();
	const clearMarks = createClearLooksWrongMutation();
	const items = $derived(query.data?.pages.flatMap((page) => page.items) ?? []);
	const total = $derived(query.data?.pages[0]?.total ?? 0);
	const busy = $derived(confirm.isPending || clearMarks.isPending);

	const EMPTY: Record<WaitingView, string> = {
		unconfirmed: 'No best guesses. Every matched album is confirmed.',
		unmatched: 'Every album matches something on MusicBrainz.',
		looks_wrong: 'Nobody has marked an album as looking wrong.'
	};

	let list = $state<HTMLUListElement | null>(null);

	function rowOf(albumId: string): HTMLElement | null {
		return list?.querySelector<HTMLElement>(`[data-album="${CSS.escape(albumId)}"]`) ?? null;
	}

	// The row after this one, to move to once this one is handled.
	function after(albumId: string): string | null {
		const at = items.findIndex((item) => item.album_id === albumId);
		return items[at + 1]?.album_id ?? null;
	}

	async function moveTo(albumId: string | null): Promise<void> {
		await tick();
		const row = albumId ? rowOf(albumId) : null;
		row?.querySelector<HTMLElement>('[data-row-focus]')?.focus();
	}

	// Brought back from an album's edition sheet: show the album after it.
	let focused = false;
	$effect(() => {
		if (focused || !focus || !items.length) return;
		focused = true;
		const row = rowOf(focus);
		if (!row) return;
		row.scrollIntoView({ block: 'center' });
		row.querySelector<HTMLElement>('[data-row-focus]')?.focus({ preventScroll: true });
	});

	function compareHref(item: WaitingAlbum): string {
		const next = after(item.album_id) ?? item.album_id;
		const params = new URLSearchParams({ edition: 'choose', from: view, next });
		return withBasePath(`/album/${encodeURIComponent(item.album_id)}?${params}`);
	}

	function failed(error: unknown, fallback: string): void {
		toastStore.show({ message: failureSentence(error, fallback), type: 'error' });
		// A stale row is read again, so the list shows the album as it is now.
		if (error instanceof ApiError && error.status === 409) void query.refetch();
	}

	async function thatsRight(item: WaitingAlbum): Promise<void> {
		const next = after(item.album_id);
		try {
			const status = await confirm.mutateAsync({
				userId: userId(),
				localId: item.album_id,
				identityRevision: item.identity_revision
			});
			toastStore.show({
				message: `Confirmed ${item.title}.`,
				type: 'success',
				duration: 8000,
				action: {
					label: 'Undo',
					run: () =>
						undo
							.mutateAsync({
								userId: userId(),
								localId: item.album_id,
								identityRevision: status.identity_revision
							})
							.catch((error: unknown) => failed(error, "The confirmation couldn't be undone."))
				}
			});
			void moveTo(next);
		} catch (error) {
			failed(error, "This album couldn't be confirmed.");
		}
	}

	async function looksFine(item: WaitingAlbum): Promise<void> {
		const next = after(item.album_id);
		try {
			await clearMarks.mutateAsync({ userId: userId(), localId: item.album_id });
			toastStore.show({ message: `Marks cleared on ${item.title}.`, type: 'success' });
			void moveTo(next);
		} catch (error) {
			failed(error, "The marks couldn't be cleared.");
		}
	}

	// An edition a curator took though no file fits it: its files wait to be
	// matched by hand, not to be found.
	const needsMatching = (item: WaitingAlbum) =>
		item.state === 'unmatched' && item.reason.code === 'CHOSEN_EDITION_FITS_NO_FILE';

	function stateLabel(item: WaitingAlbum): string {
		if (needsMatching(item)) return NEEDS_MATCHING;
		if (item.state === 'unmatched') return 'Not found';
		if (item.state === 'unconfirmed') return 'Best guess';
		if (item.state === 'chosen') return 'Chosen by a person';
		if (item.state === 'confirmed') return 'Picked by DroppedNeedle';
		return 'Not matched yet';
	}
</script>

<div class="space-y-3">
	<QueryBoundary {query} what="the list" isEmpty={() => items.length === 0}>
		{#snippet loading()}
			<div class="space-y-2">
				{#each Array(6) as _, index (index)}<div class="skeleton h-24 w-full"></div>{/each}
			</div>
		{/snippet}
		{#snippet empty()}
			<Card class="text-center text-base-content/60">{EMPTY[view]}</Card>
		{/snippet}
		{#snippet children(loaded)}
			<p class="text-sm text-base-content/60">
				{count(loaded.pages[0]?.total ?? 0, 'album', 'albums')}
			</p>
			<ul class="space-y-2" bind:this={list}>
				{#each items as item (item.album_id)}
					{@const guess = item.guess}
					<li
						data-album={item.album_id}
						class="grid gap-3 rounded-box border border-base-content/10 bg-base-100 p-3 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] lg:items-center"
					>
						<div class="flex min-w-0 items-start gap-3">
							<AlbumImage
								albumId={item.album_id}
								alt=""
								size="sm"
								rounded="md"
								className="h-14 w-14 shrink-0 border border-base-content/10"
							/>
							<div class="min-w-0">
								<p class="text-xs font-semibold tracking-wide text-base-content/50 uppercase">
									Your files
								</p>
								<a
									data-row-focus
									class="block truncate font-medium hover:underline"
									href={withBasePath(`/album/${encodeURIComponent(item.album_id)}`)}>{item.title}</a
								>
								<p class="truncate text-xs text-base-content/55">
									{[item.artist_name, count(item.file_count, 'file', 'files')]
										.filter(Boolean)
										.join(' · ')}
								</p>
							</div>
						</div>

						<div class="flex min-w-0 items-start gap-3">
							{#if guess}
								<AlbumImage
									customUrl={guess.release_mbid
										? coverPath('release', guess.release_mbid, 250)
										: undefined}
									mbid={guess.release_group_mbid}
									alt=""
									size="sm"
									rounded="md"
									className="h-14 w-14 shrink-0 border border-base-content/10"
									retryOnError={false}
								/>
							{/if}
							<div class="min-w-0">
								<p
									class="flex items-center gap-1.5 text-xs font-semibold tracking-wide uppercase {item.state ===
										'unmatched' && !needsMatching(item)
										? 'text-error'
										: 'text-warning'}"
								>
									{#if item.state === 'unmatched' && !needsMatching(item)}
										<CircleAlert class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
									{:else}
										<CircleHelp class="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
									{/if}
									{stateLabel(item)}
								</p>
								{#if guess}
									<p class="truncate font-medium">{guess.title}</p>
									<p class="truncate text-xs text-base-content/55">
										{[guess.artist_name, fitLine(guess.matched_files, item.file_count)]
											.filter(Boolean)
											.join(' · ')}
									</p>
								{/if}
								<p class="mt-1 text-sm text-base-content/70">{item.reason.message}</p>
								{#if item.looks_wrong_count > 0}
									<p class="mt-1 flex items-center gap-1 text-xs text-base-content/60">
										<Flag class="h-3 w-3 shrink-0" aria-hidden="true" />
										{item.looks_wrong_count === 1
											? '1 person says it looks wrong'
											: `${item.looks_wrong_count} people say it looks wrong`}
									</p>
								{/if}
							</div>
						</div>

						<div class="flex flex-wrap gap-2 lg:justify-end">
							{#if authStore.isTrusted && item.state === 'unconfirmed'}
								<Button
									variant="primary"
									size="sm"
									disabled={busy}
									aria-label={`That's right: ${item.title} is ${guess?.title ?? 'this edition'}`}
									onclick={() => void thatsRight(item)}>That's right</Button
								>
							{/if}
							{#if authStore.isTrusted && view === 'looks_wrong'}
								<Button
									variant="ghost"
									size="sm"
									disabled={busy}
									onclick={() => void looksFine(item)}>Looks fine</Button
								>
							{/if}
							{#if authStore.isTrusted}
								<Button variant="ghost" size="sm" href={compareHref(item)}
									>{needsMatching(item)
										? MATCH_FILES
										: item.state === 'unmatched'
											? 'Pick an edition'
											: 'Compare'}</Button
								>
							{/if}
						</div>
					</li>
				{/each}
			</ul>
			{#if query.hasNextPage}
				<div class="flex items-center justify-between text-sm">
					<span class="text-base-content/60">{items.length} of {total}</span>
					<Button
						size="sm"
						loading={query.isFetchingNextPage}
						onclick={() => void query.fetchNextPage()}>Load more</Button
					>
				</div>
			{/if}
		{/snippet}
	</QueryBoundary>
</div>
