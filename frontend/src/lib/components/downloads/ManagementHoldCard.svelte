<script lang="ts">
	import {
		ArchiveRestore,
		ChevronDown,
		FolderLock,
		RefreshCw,
		Settings2,
		Trash2,
		X
	} from 'lucide-svelte';

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
	const reasonCode = $derived(first?.reason.replace(/^management:/, '') ?? 'UNKNOWN');
	const detail = $derived(first?.reason_detail?.trim() || null);
	const nextRetryAt = $derived(first?.management_next_retry_at ?? null);
	const nextRetryLabel = $derived.by(() => {
		if (!nextRetryAt) return null;
		return new Date(nextRetryAt * 1000).toLocaleTimeString([], {
			hour: '2-digit',
			minute: '2-digit'
		});
	});
	let filesOpen = $state(false);
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
		void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.stats() });
		void invalidateQueriesWithPersister({ queryKey: LibraryQueryKeyFactory.recentlyAdded() });
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

	const reason = $derived.by(() => {
		switch (reasonCode) {
			case 'METADATA_UNAVAILABLE':
				return 'Required metadata was temporarily unavailable. Your download is intact and can be retried without fetching it again.';
			case 'FORMAT_UNSUPPORTED':
				return 'This audio format does not have a tested write adapter. The original download was left untouched.';
			case 'FIELD_UNSUPPORTED_BY_FORMAT':
				return 'The active profile asked this audio format to make a change its adapter could not represent safely.';
			case 'SCRIPT_VALIDATION_FAILED':
				return "The active profile's tagging or naming rules could not process this file safely.";
			case 'PROFILE_CHANGED':
			case 'POLICY_CHANGED':
				return 'The file organization configuration changed while this album was being prepared. Review it, then retry.';
			case 'PATH_COLLISION_DIFFERENT':
			case 'POSITION_COLLISION':
			case 'SIDECAR_COLLISION':
				return 'A planned destination conflicts with a different file. Nothing was overwritten.';
			case 'ROOT_UNAVAILABLE':
			case 'ROOT_READ_ONLY':
				return 'The library destination was unavailable or read-only when DroppedNeedle tried to publish the album.';
			case 'INSUFFICIENT_SPACE':
				return 'There was not enough temporary or destination space to publish this album safely.';
			case 'OUT_OF_ROOT':
			case 'PATH_TOO_LONG':
			case 'SYMLINK_UNSUPPORTED':
				return 'A path or sidecar did not pass the organizer’s filesystem safety rules.';
			case 'BUNDLE_TOO_LARGE':
				return 'This acquisition contains too many files or sidecars to organize automatically as one safe unit.';
			case 'BUNDLE_BLOCKED':
			case 'RECOVERY_NEEDS_ATTENTION':
			case 'RECYCLE_UNAVAILABLE':
				return 'The durable writer could not safely commit or recover this album. Review file organization, then retry.';
			case 'TRACK_NOT_MAPPED':
				return 'The exact release-track identity was no longer available when publication began.';
			default:
				return 'File organization stopped before writing because one of its safety checks needs attention.';
		}
	});

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
	<article class="rounded-3xl border border-base-content/10 bg-base-200/60 p-4 sm:p-5">
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
					Download secured · organizer paused
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
				{#if nextRetryLabel && !retry.isPending && !retryRunning}
					<p class="mt-2 text-xs font-semibold text-info" role="status">
						Automatic organizer retry scheduled for {nextRetryLabel}. You can retry now instead.
					</p>
				{/if}

				<div class="mt-4 flex flex-wrap items-center gap-2">
					{#if canManage}
						<button
							type="button"
							class="btn btn-primary btn-sm"
							onclick={retryUnit}
							disabled={busy || !taskId}
						>
							<RefreshCw
								class="size-4 {retry.isPending || retryRunning
									? 'animate-spin motion-reduce:animate-none'
									: ''}"
								aria-hidden="true"
							/>
							{retry.isPending
								? 'Retrying organizer…'
								: retryRunning
									? 'Retry in progress…'
									: 'Retry organizer'}
						</button>
						<a
							href={withBasePath('/library/management?tab=automation')}
							class="btn btn-ghost btn-sm"
						>
							<Settings2 class="size-4" aria-hidden="true" /> Review automation
						</a>
						<button
							type="button"
							class="btn btn-ghost btn-sm text-base-content/55 hover:text-error"
							onclick={(event) => requestDiscard(event.currentTarget)}
							disabled={busy}
						>
							<Trash2 class="size-4" aria-hidden="true" /> Discard download
						</button>
					{:else}
						<span class="px-2 text-xs text-base-content/50">
							An administrator can retry, discard, or review this organizer hold.
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

				<div class="mt-4 border-t border-base-content/8 pt-3">
					<button
						type="button"
						class="flex w-full items-center justify-between gap-3 text-left text-xs font-semibold text-base-content/60 hover:text-base-content"
						onclick={() => (filesOpen = !filesOpen)}
						aria-expanded={filesOpen}
					>
						<span
							>{filesOpen ? 'Hide secured files' : 'Show secured files and technical detail'}</span
						>
						<ChevronDown
							class="size-4 transition-transform motion-reduce:transition-none {filesOpen
								? 'rotate-180'
								: ''}"
							aria-hidden="true"
						/>
					</button>
					{#if filesOpen}
						<div class="mt-3 grid gap-3 lg:grid-cols-[minmax(0,1fr)_minmax(14rem,0.55fr)]">
							<ol class="max-h-52 space-y-1 overflow-y-auto pr-2 text-xs text-base-content/65">
								{#each sorted as item (item.id)}
									<li class="flex gap-2 rounded-lg bg-base-300/35 px-2.5 py-2">
										<span class="w-7 shrink-0 text-right font-mono text-base-content/35"
											>{item.track_number ?? '-'}</span
										>
										<span class="min-w-0 truncate"
											>{item.track_title ?? item.original_filename ?? 'Unknown file'}</span
										>
									</li>
								{/each}
							</ol>
							<div class="rounded-xl bg-base-300/35 p-3 text-xs text-base-content/60">
								<p class="font-bold uppercase tracking-[0.12em] text-base-content/40">
									Safety gate
								</p>
								<p class="mt-1 font-mono text-[11px] break-all">{reasonCode}</p>
								{#if detail}<p class="mt-2 leading-relaxed">{detail}</p>{/if}
							</div>
						</div>
					{/if}
				</div>
			</div>
		</div>
	</article>

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
					<button class="btn btn-circle btn-ghost btn-sm" aria-label="Close">
						<X class="size-4" aria-hidden="true" />
					</button>
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
				<form method="dialog"><button class="btn btn-ghost">Keep files</button></form>
				<button class="btn btn-error" onclick={discardUnit} disabled={discard.isPending}>
					<Trash2 class="size-4" aria-hidden="true" />
					{discard.isPending ? 'Discarding…' : 'Discard secured files'}
				</button>
			</div>
		</div>
		<form method="dialog" class="modal-backdrop"><button aria-label="Close">close</button></form>
	</dialog>
{/if}
