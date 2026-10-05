<script lang="ts">
	import { ChevronDown, Disc3, RefreshCw, Trash2, X } from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import {
		discardHeldVerdict,
		retryDownload
	} from '$lib/queries/downloads/DownloadMutations.svelte';
	import type { DownloadTask, HeldImport } from '$lib/types';
	import { albumHref } from '$lib/utils/entityRoutes';

	import HeldTrackCard from './HeldTrackCard.svelte';

	interface Props {
		task: DownloadTask;
		items: HeldImport[];
	}

	let { task, items }: Props = $props();

	const discard = discardHeldVerdict();
	const retry = retryDownload();
	const releaseGroupMbid = $derived(task.release_group_mbid ?? null);
	const busy = $derived(discard.isPending || retry.isPending);
	const sorted = $derived(
		[...items].sort(
			(a, b) =>
				(a.disc_number ?? 1) - (b.disc_number ?? 1) ||
				(a.track_number ?? 0) - (b.track_number ?? 0) ||
				a.id - b.id
		)
	);
	const grabbed = $derived(task.wrong_product_detail?.trim() || null);
	let tracksOpen = $state(false);
	let discardDialog = $state<HTMLDialogElement | null>(null);
	let discardHeading = $state<HTMLHeadingElement | null>(null);
	let discardOpener = $state<HTMLButtonElement | null>(null);
	let discardError = $state<string | null>(null);

	function requestDiscard(opener: HTMLButtonElement): void {
		discardError = null;
		discardOpener = opener;
		discardDialog?.showModal();
		discardHeading?.focus();
	}

	function restoreDiscardFocus(): void {
		discardOpener?.focus();
	}

	function discardUnit(): void {
		discardError = null;
		discard.mutate(
			{ taskId: task.id },
			{
				onSuccess: () => discardDialog?.close(),
				onError: (error: unknown) => {
					discardError =
						error instanceof Error && error.message
							? error.message
							: 'The held files could not be discarded.';
				}
			}
		);
	}
</script>

<article class="rounded-3xl border border-warning/25 bg-base-200/60 p-4 sm:p-5">
	<div class="flex items-start gap-4">
		<div
			class="relative size-16 shrink-0 overflow-hidden rounded-2xl ring-1 ring-base-content/10 sm:size-20"
		>
			{#if releaseGroupMbid}
				<AlbumImage
					mbid={releaseGroupMbid}
					alt={task.album_title ?? 'Held album'}
					size="sm"
					rounded="xl"
					className="h-full w-full"
				/>
			{:else}
				<div class="grid h-full w-full place-items-center bg-base-300">
					<Disc3 class="size-7 text-warning" aria-hidden="true" />
				</div>
			{/if}
			<span
				class="absolute right-1 bottom-1 grid size-6 place-items-center rounded-full bg-warning text-warning-content shadow-sm"
			>
				<Disc3 class="size-3.5" aria-hidden="true" />
			</span>
		</div>

		<div class="min-w-0 flex-1">
			<p
				class="inline-flex items-center gap-1.5 rounded-full bg-warning/10 px-2.5 py-1 text-[11px] font-bold uppercase tracking-[0.14em] text-warning ring-1 ring-warning/20 ring-inset"
			>
				<Disc3 class="size-3.5" aria-hidden="true" />
				Wrong edition grabbed
			</p>
			<h3 class="mt-1 truncate text-lg font-black tracking-tight">
				{#if releaseGroupMbid}
					<a
						href={albumHref(releaseGroupMbid)}
						class="transition-colors hover:text-primary motion-reduce:transition-none"
					>
						{task.album_title}
					</a>
				{:else}
					{task.album_title}
				{/if}
			</h3>
			<p class="text-sm text-base-content/60">
				{task.artist_name} · {items.length}
				{items.length === 1 ? 'file' : 'files'} held, none imported
			</p>
			<p class="mt-3 max-w-3xl text-sm leading-relaxed text-base-content/70">
				{#if grabbed}
					Grabbed “{grabbed}”, but it doesn't match the expected
					<strong class="text-base-content">{task.album_title}</strong> edition, so every file failed
					verification. Discard the lot, retry the hunt, or review tracks one by one below.
				{:else}
					The downloaded files don't match the expected
					<strong class="text-base-content">{task.album_title}</strong> edition, so every file failed
					verification. Discard the lot, retry the hunt, or review tracks one by one below.
				{/if}
			</p>

			<div class="mt-4 flex flex-wrap items-center gap-2">
				<button
					type="button"
					class="btn btn-primary btn-sm"
					onclick={() => retry.mutate(task.id)}
					disabled={busy}
				>
					<RefreshCw
						class="size-4 {retry.isPending ? 'animate-spin motion-reduce:animate-none' : ''}"
						aria-hidden="true"
					/>
					{retry.isPending ? 'Retrying…' : 'Retry download'}
				</button>
				<button
					type="button"
					class="btn btn-ghost btn-sm text-base-content/55 hover:text-error"
					onclick={(event) => requestDiscard(event.currentTarget)}
					disabled={busy}
				>
					<Trash2 class="size-4" aria-hidden="true" /> Discard all {items.length}
				</button>
			</div>

			<div class="mt-4 border-t border-base-content/8 pt-3">
				<button
					type="button"
					class="flex w-full items-center justify-between gap-3 text-left text-xs font-semibold text-base-content/60 hover:text-base-content"
					onclick={() => (tracksOpen = !tracksOpen)}
					aria-expanded={tracksOpen}
				>
					<span>{tracksOpen ? 'Hide held tracks' : 'Review tracks one by one'}</span>
					<ChevronDown
						class="size-4 transition-transform motion-reduce:transition-none {tracksOpen
							? 'rotate-180'
							: ''}"
						aria-hidden="true"
					/>
				</button>
				{#if tracksOpen}
					<div class="mt-3 space-y-3">
						{#each sorted as item (item.id)}
							<HeldTrackCard held={item} />
						{/each}
					</div>
				{/if}
			</div>
		</div>
	</div>
</article>

<dialog
	bind:this={discardDialog}
	class="modal"
	aria-labelledby="verdict-discard-title"
	onclose={restoreDiscardFocus}
>
	<div class="modal-box max-w-lg">
		<div class="flex items-start justify-between gap-3">
			<div>
				<p class="text-xs font-bold uppercase tracking-[0.16em] text-error">Permanent deletion</p>
				<h3
					bind:this={discardHeading}
					id="verdict-discard-title"
					tabindex="-1"
					class="mt-1 text-xl font-black outline-none"
				>
					Discard these held files?
				</h3>
			</div>
			<form method="dialog">
				<button class="btn btn-circle btn-ghost btn-sm" aria-label="Close">
					<X class="size-4" aria-hidden="true" />
				</button>
			</form>
		</div>
		<p class="mt-4 text-sm leading-relaxed text-base-content/65">
			This permanently deletes all {items.length} held {items.length === 1 ? 'file' : 'files'} for
			<strong class="text-base-content">{task.album_title}</strong>. It will not change files
			already in your library.
		</p>
		{#if discardError}
			<div class="alert alert-error mt-4 text-sm" role="alert">{discardError}</div>
		{/if}
		<div class="modal-action">
			<form method="dialog"><button class="btn btn-ghost">Keep files</button></form>
			<button class="btn btn-error" onclick={discardUnit} disabled={discard.isPending}>
				<Trash2 class="size-4" aria-hidden="true" />
				{discard.isPending ? 'Discarding…' : 'Discard held files'}
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop"><button aria-label="Close">close</button></form>
</dialog>
