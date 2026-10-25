<script lang="ts">
	import { ArchiveRestore, FolderLock, RefreshCw, Settings2, Trash2, X } from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import {
		createDownloadStream,
		getOrganizerRetry,
		resetOrganizerRetry,
		type OrganizerRetryStage
	} from '$lib/queries/downloads/DownloadSSE.svelte';
	import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
	import {
		discardHeldManagementUnit,
		retryHeldManagementUnit
	} from '$lib/queries/downloads/DownloadMutations.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { HeldImport } from '$lib/types';
	import { withBasePath } from '$lib/utils/basePath';
	import { albumHref } from '$lib/utils/entityRoutes';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	import HeldTrackCard from './HeldTrackCard.svelte';

	interface Props {
		items: HeldImport[];
	}

	let { items }: Props = $props();

	const retry = retryHeldManagementUnit();
	const discard = discardHeldManagementUnit();
	const retryStream = createDownloadStream();
	const first = $derived(items[0]);
	const taskId = $derived(first?.source_task_id ?? '');
	const releaseGroupMbid = $derived(first?.release_group_mbid ?? null);
	// The task-keyed organizer_retry snapshot: live while this card's own mutation
	// runs, and re-attached on mount when another session started the retry.
	const retrySnapshot = $derived(taskId ? getOrganizerRetry(taskId) : null);
	const retryRunning = $derived(retrySnapshot?.state === 'running');
	const busy = $derived(retry.isPending || discard.isPending || retryRunning);

	const STAGE_LABELS: Record<OrganizerRetryStage, string> = {
		preparing: 'Preparing',
		planning: 'Planning',
		publishing: 'Publishing',
		finalizing: 'Finalizing'
	};
	// Non-empty-guarded: every label below renders only when it has real content,
	// so a partial snapshot can never paint a blank line.
	const stageLabel = $derived(
		retrySnapshot ? (STAGE_LABELS[retrySnapshot.stage] ?? 'Working') : ''
	);
	const progressCompleted = $derived(retrySnapshot?.files_completed ?? 0);
	const progressTotal = $derived(retrySnapshot?.files_total ?? 0);
	const progressLine = $derived(
		retrySnapshot?.state === 'running' && stageLabel
			? `${stageLabel} ${progressCompleted}/${progressTotal}`
			: ''
	);
	const canManage = $derived(authStore.isAdmin);
	const sorted = $derived(
		[...items].sort(
			(a, b) =>
				(a.disc_number ?? 1) - (b.disc_number ?? 1) ||
				(a.track_number ?? 0) - (b.track_number ?? 0) ||
				a.id - b.id
		)
	);
	const detail = $derived(first?.reason_detail?.trim() || null);
	const nextRetryAt = $derived(first?.management_next_retry_at ?? null);
	const nextRetryLabel = $derived.by(() => {
		if (!nextRetryAt) return null;
		return new Date(nextRetryAt * 1000).toLocaleTimeString([], {
			hour: '2-digit',
			minute: '2-digit'
		});
	});
	let discardDialog = $state<HTMLDialogElement | null>(null);
	let discardHeading = $state<HTMLHeadingElement | null>(null);
	let discardOpener = $state<HTMLButtonElement | null>(null);
	let discardError = $state<string | null>(null);
	let retryError = $state<string | null>(null);
	let retryDetailAtAttempt = $state<string | null>(null);
	let showRetryStatus = $state(false);
	// Owned attempt bookkeeping: the mutation response and the terminal SSE event
	// race, so the first to arrive settles the UI and the other is a no-op.
	let retryOwned = $state(false);
	let retrySettled = $state(false);
	let retryImported = $state<number | null>(null);
	const completeLine = $derived(
		retryOwned && retrySettled && retryImported !== null
			? `Organizer retry imported ${retryImported} ${retryImported === 1 ? 'file' : 'files'}.`
			: ''
	);
	const awaitingFirstEvent = $derived(retry.isPending && !retrySnapshot);
	const retryFailed = $derived(retrySettled && !completeLine);
	const retryBoxTone = $derived(
		completeLine
			? 'border-success/25 bg-success/8 text-success'
			: retryFailed
				? 'border-error/25 bg-error/8 text-error'
				: 'border-info/25 bg-info/8 text-info'
	);

	// Subscribe for the attempt's live snapshots and for mount re-attach: the
	// backend replays the latest organizer_retry snapshot on connect, so a refresh
	// mid-retry resumes progress instead of starting a duplicate.
	$effect(() => {
		if (!taskId) return;
		retryStream.start(taskId);
		return () => retryStream.stop();
	});

	$effect(() => {
		if (retryError && detail !== retryDetailAtAttempt) {
			retryError = null;
			retryDetailAtAttempt = detail;
		}
	});

	// Mirror of the retry mutation's invalidation surface for terminal snapshots
	// that arrive without an owning mutation (refresh-orphaned or foreign tab):
	// silent, no toast - the outcome belongs to another session.
	function settleForeignTerminal(): void {
		resetOrganizerRetry(taskId);
		void invalidateQueriesWithPersister({
			queryKey: DownloadQueryKeyFactory.tasks(authStore.user?.id)
		});
		void invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.catalog.stats(authStore.user?.id)
		});
		void invalidateQueriesWithPersister({
			queryKey: LibraryQueryKeyFactory.catalog.recentlyAddedPrefix(authStore.user?.id)
		});
		if (releaseGroupMbid) {
			void invalidateQueriesWithPersister({
				queryKey: LibraryQueryKeyFactory.album(releaseGroupMbid)
			});
		}
	}

	$effect(() => {
		const snapshot = retrySnapshot;
		if (!snapshot || snapshot.state === 'running') return;
		if (!retryOwned) {
			settleForeignTerminal();
			return;
		}
		if (retrySettled) return;
		retrySettled = true;
		showRetryStatus = true;
		if (snapshot.state === 'complete') {
			retryImported = snapshot.files_imported ?? snapshot.files_completed ?? 0;
		} else {
			retryError = snapshot.error?.trim() || 'File organization still needs attention.';
			retryDetailAtAttempt = detail;
		}
	});

	// The server words every reason: what happened and what to do.
	const reason = $derived(first?.reason_text ?? '');
	const action = $derived(first?.reason_action ?? '');

	function retryUnit(): void {
		if (!taskId) return;
		retry.reset();
		// A stale terminal snapshot would reject the new attempt's events, so the
		// entry resets up front; the stream (opened on mount) stays subscribed.
		resetOrganizerRetry(taskId);
		retryError = null;
		retryDetailAtAttempt = detail;
		retryOwned = true;
		retrySettled = false;
		retryImported = null;
		showRetryStatus = true;
		retry.mutate(
			{ taskId, releaseGroupMbid },
			{
				onSuccess: (data) => {
					// The mutation's own handlers already toasted + invalidated; the
					// entry reset accompanies that invalidation, and the UI settles
					// here only if the terminal event has not beaten the response.
					resetOrganizerRetry(taskId);
					if (retrySettled) return;
					retrySettled = true;
					retryImported = data?.files ?? 0;
				},
				onError: (error: unknown) => {
					// 409 already-running and every other failure surface through the
					// existing inline pattern with the backend message verbatim.
					resetOrganizerRetry(taskId);
					if (retrySettled) return;
					retrySettled = true;
					retryError =
						error instanceof Error && error.message
							? error.message
							: 'File organization still needs attention.';
				}
			}
		);
	}

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
		if (!taskId) return;
		discardError = null;
		discard.mutate(
			{ taskId, releaseGroupMbid },
			{
				onSuccess: () => discardDialog?.close(),
				onError: (error: unknown) => {
					discardError =
						error instanceof Error && error.message
							? error.message
							: 'The secured files could not be discarded.';
				}
			}
		);
	}
</script>

{#if first}
	<Card as="article" variant="bordered" padding="md">
		<div class="flex items-start gap-4">
			<div
				class="relative size-16 shrink-0 overflow-hidden rounded-2xl ring-1 ring-base-content/10 sm:size-20"
			>
				{#if releaseGroupMbid}
					<AlbumImage
						mbid={releaseGroupMbid}
						alt={first.album_title ?? 'Held album'}
						size="sm"
						rounded="xl"
						className="h-full w-full"
					/>
				{:else}
					<div class="grid h-full w-full place-items-center bg-base-300">
						<FolderLock class="size-7 text-warning" aria-hidden="true" />
					</div>
				{/if}
				<span
					class="absolute right-1 bottom-1 grid size-6 place-items-center rounded-full bg-warning text-warning-content shadow-sm"
				>
					<ArchiveRestore class="size-3.5" aria-hidden="true" />
				</span>
			</div>

			<div class="min-w-0 flex-1">
				<p
					class="inline-flex items-center gap-1.5 rounded-full bg-warning/10 px-2.5 py-1 text-[11px] font-bold uppercase tracking-[0.14em] text-warning ring-1 ring-warning/20 ring-inset"
				>
					<FolderLock class="size-3.5" aria-hidden="true" />
					Download kept · waiting on the library
				</p>
				<h3 class="mt-1 truncate text-lg font-black tracking-tight">
					{#if releaseGroupMbid}
						<a
							href={albumHref(releaseGroupMbid)}
							class="transition-colors hover:text-primary motion-reduce:transition-none"
						>
							{first.album_title ?? 'Downloaded album'}
						</a>
					{:else}
						{first.album_title ?? 'Downloaded album'}
					{/if}
				</h3>
				<p class="text-sm text-base-content/60">
					{first.artist_name ?? 'Unknown artist'} · {items.length}
					{items.length === 1 ? 'file' : 'files'} safely held
				</p>
				<p class="mt-3 max-w-3xl text-sm leading-relaxed text-base-content/70">{reason}</p>
				{#if action}
					<p class="mt-1 max-w-3xl text-sm text-base-content/60">{action}</p>
				{/if}
				{#if nextRetryLabel && !retry.isPending && !retryRunning}
					<p class="mt-2 text-xs font-semibold text-info" role="status">
						Automatic organizer retry scheduled for {nextRetryLabel}. You can retry now instead.
					</p>
				{/if}

				<div class="mt-4 flex flex-wrap items-center gap-2">
					{#if canManage}
						<Button
							variant="primary"
							size="sm"
							icon={RefreshCw}
							loading={retry.isPending || retryRunning}
							onclick={retryUnit}
							disabled={busy || !taskId}
						>
							{retry.isPending
								? 'Retrying organizer…'
								: retryRunning
									? 'Retry in progress…'
									: 'Retry organizer'}
						</Button>
						<Button
							variant="ghost"
							size="sm"
							icon={Settings2}
							href={withBasePath('/library/management?tab=automation')}
						>
							Review automation
						</Button>
						<Button
							variant="ghost"
							size="sm"
							icon={Trash2}
							onclick={(event: MouseEvent) =>
								requestDiscard(event.currentTarget as HTMLButtonElement)}
							disabled={busy}
						>
							Discard download
						</Button>
					{:else}
						<span class="px-2 text-xs text-base-content/50">
							An administrator can retry or discard the whole download.
						</span>
					{/if}
				</div>
				{#if showRetryStatus || retryRunning}
					<div
						class="mt-3 rounded-xl border px-3 py-2.5 text-sm {retryBoxTone}"
						role={retryFailed ? 'alert' : 'status'}
						aria-live="polite"
					>
						{#if completeLine}
							<p class="font-bold">{completeLine}</p>
						{:else if progressLine}
							<p class="font-bold">{progressLine}</p>
							<progress
								class="progress progress-info mt-2 w-full"
								value={progressCompleted}
								max={Math.max(progressTotal, 1)}
								aria-label={progressLine}
							>
								{progressLine}
							</progress>
						{:else if awaitingFirstEvent}
							<p class="font-bold">Rechecking the secured album…</p>
						{:else}
							<p class="font-bold">Organizer still paused</p>
							{#if retryError || detail}
								<p class="mt-0.5 leading-relaxed">{retryError ?? detail}</p>
							{/if}
						{/if}
					</div>
				{/if}

				<div class="mt-4 space-y-3 border-t border-base-content/8 pt-3">
					{#each sorted as item (item.id)}
						<HeldTrackCard held={item} />
					{/each}
				</div>
			</div>
		</div>
	</Card>

	<dialog
		bind:this={discardDialog}
		class="modal"
		aria-labelledby="management-hold-discard-title"
		onclose={restoreDiscardFocus}
	>
		<div class="modal-box max-w-lg">
			<div class="flex items-start justify-between gap-3">
				<div>
					<p class="text-xs font-bold uppercase tracking-[0.16em] text-error">Permanent deletion</p>
					<h3
						bind:this={discardHeading}
						id="management-hold-discard-title"
						tabindex="-1"
						class="mt-1 text-xl font-black outline-none"
					>
						Discard this downloaded album?
					</h3>
				</div>
				<form method="dialog">
					<IconButton type="submit" size="sm" icon={X} label="Close" />
				</form>
			</div>
			<p class="mt-4 text-sm leading-relaxed text-base-content/65">
				This permanently deletes all {items.length} secured {items.length === 1 ? 'file' : 'files'} for
				<strong class="text-base-content">{first.album_title ?? 'this album'}</strong>. It will not
				change files already in your library.
			</p>
			{#if discardError}
				<div class="alert alert-error mt-4 text-sm" role="alert">{discardError}</div>
			{/if}
			<div class="modal-action">
				<form method="dialog"><Button type="submit" variant="ghost">Keep files</Button></form>
				<Button variant="danger" icon={Trash2} onclick={discardUnit} disabled={discard.isPending}>
					{discard.isPending ? 'Discarding…' : 'Discard secured files'}
				</Button>
			</div>
		</div>
		<form method="dialog" class="modal-backdrop"><button aria-label="Close">close</button></form>
	</dialog>
{/if}
