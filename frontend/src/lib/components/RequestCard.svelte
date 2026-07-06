<script lang="ts">
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import AlbumImage from './AlbumImage.svelte';
	import DeleteAlbumModal from './DeleteAlbumModal.svelte';
	import ArtistRemovedModal from './ArtistRemovedModal.svelte';
	import type { ActiveRequestItem, RequestHistoryItem } from '$lib/types';
	import { reimportDownload } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		ChevronDown,
		X,
		RotateCcw,
		Trash2,
		Clock,
		Download,
		FileDown,
		Loader,
		CheckCircle2,
		XCircle,
		AlertTriangle,
		Ban,
		Pause,
		Music,
		Radar,
		Upload
	} from 'lucide-svelte';

	interface Props {
		item: ActiveRequestItem | RequestHistoryItem;
		mode: 'active' | 'history';
		// still being worked on despite the terminal status: 'retrying' = auto-retry
		// ladder, 'watching' = wanted watcher. Renders a chip linking to the Wanted tab.
		watchState?: 'retrying' | 'watching';
		oncancel?: (mbid: string) => void;
		onretry?: (mbid: string) => void;
		onclear?: (mbid: string) => void;
		onremoved?: () => void;
		onreimported?: () => void;
	}

	let { item, mode, watchState, oncancel, onretry, onclear, onremoved, onreimported }: Props =
		$props();

	const reimport = reimportDownload();

	let confirmingCancel = $state(false);
	let showDeleteModal = $state(false);
	let showArtistRemovedModal = $state(false);
	let removedArtistName = $state('');

	function formatRelativeTime(dateStr: string): string {
		const date = new Date(dateStr);
		const now = new Date();
		const diffMs = now.getTime() - date.getTime();
		const diffMin = Math.floor(diffMs / 60000);
		if (diffMin < 1) return 'just now';
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHr = Math.floor(diffMin / 60);
		if (diffHr < 24) return `${diffHr}h ago`;
		const diffDays = Math.floor(diffHr / 24);
		if (diffDays < 7) return `${diffDays}d ago`;
		return date.toLocaleDateString();
	}

	function formatDate(dateStr: string): string {
		return new Date(dateStr).toLocaleString(undefined, {
			month: 'short',
			day: 'numeric',
			year: 'numeric',
			hour: 'numeric',
			minute: '2-digit'
		});
	}

	function formatSize(bytes: number): string {
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
		if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
		return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
	}

	function formatEta(etaStr: string): string {
		const eta = new Date(etaStr);
		const now = new Date();
		const diffMs = eta.getTime() - now.getTime();
		if (diffMs <= 0) return 'any moment';
		const diffMin = Math.floor(diffMs / 60000);
		if (diffMin < 1) return '< 1 min';
		if (diffMin < 60) return `${diffMin} min`;
		const diffHr = Math.floor(diffMin / 60);
		const remainMin = diffMin % 60;
		return remainMin > 0 ? `${diffHr}h ${remainMin}m` : `${diffHr}h`;
	}

	type StatusConfig = {
		badgeClass: string;
		label: string;
		icon: typeof Clock;
		glow?: string;
	};

	const statusConfig = $derived.by((): StatusConfig => {
		switch (item.status) {
			case 'pending':
				return {
					badgeClass: 'badge-warning',
					label: 'Searching',
					icon: Clock,
					glow: 'shadow-warning/20'
				};
			case 'queued':
				return { badgeClass: 'badge-warning', label: 'Queued', icon: Clock };
			case 'downloading':
				return {
					badgeClass: 'badge-info',
					label: 'Downloading',
					icon: Download,
					glow: 'shadow-info/20'
				};
			case 'importing':
				return { badgeClass: 'badge-info', label: 'Importing', icon: Upload };
			case 'paused':
				return { badgeClass: 'badge-warning', label: 'Paused', icon: Pause };
			case 'downloadClientUnavailable':
				return { badgeClass: 'badge-error', label: 'Client Unavailable', icon: AlertTriangle };
			case 'importFailed':
				return { badgeClass: 'badge-error', label: 'Import Failed', icon: XCircle };
			case 'importBlocked':
				return { badgeClass: 'badge-warning', label: 'Import Blocked', icon: AlertTriangle };
			case 'incomplete':
				return { badgeClass: 'badge-warning', label: 'Incomplete', icon: AlertTriangle };
			case 'imported':
				return { badgeClass: 'badge-success', label: 'Imported', icon: CheckCircle2 };
			case 'failed':
				return { badgeClass: 'badge-error', label: 'Failed', icon: XCircle };
			case 'cancelled':
				return { badgeClass: 'badge-ghost', label: 'Cancelled', icon: Ban };
			case 'awaiting_approval':
				return { badgeClass: 'badge-warning', label: 'Awaiting Approval', icon: Clock };
			case 'rejected':
				return { badgeClass: 'badge-error', label: 'Rejected', icon: Ban };
			default:
				return { badgeClass: 'badge-ghost', label: item.status, icon: Music };
		}
	});

	let showStatusDetails = $state(false);
	const isFailedState = $derived(
		item.status === 'importFailed' ||
			item.status === 'importBlocked' ||
			item.status === 'failed' ||
			item.status === 'downloadClientUnavailable'
	);
	const artistMbid = $derived('artist_mbid' in item ? item.artist_mbid : null);

	function handleCancelClick(e: Event) {
		e.stopPropagation();
		if (confirmingCancel) {
			oncancel?.(item.musicbrainz_id);
			confirmingCancel = false;
		} else {
			confirmingCancel = true;
			setTimeout(() => {
				confirmingCancel = false;
			}, 3000);
		}
	}

	function handleCancelNo(e: Event) {
		e.stopPropagation();
		confirmingCancel = false;
	}

	function handleRetry(e: Event) {
		e.stopPropagation();
		onretry?.(item.musicbrainz_id);
	}

	function handleClear(e: Event) {
		e.stopPropagation();
		onclear?.(item.musicbrainz_id);
	}

	function handleRemoveFromLibrary(e: Event) {
		e.stopPropagation();
		showDeleteModal = true;
	}

	function handleDeleted(result: { artist_removed: boolean; artist_name?: string | null }) {
		showDeleteModal = false;
		if (result.artist_removed && result.artist_name) {
			removedArtistName = result.artist_name;
			showArtistRemovedModal = true;
		}
		onremoved?.();
	}

	const isActive = $derived(mode === 'active');
	const activeItem = $derived(item as ActiveRequestItem);
	const historyItem = $derived(item as RequestHistoryItem);
	const hasProgress = $derived(
		isActive &&
			(activeItem.status === 'downloading' ||
				activeItem.status === 'importing' ||
				activeItem.status === 'paused')
	);
	const hasStatusMessages = $derived(
		isActive && activeItem.status_messages && activeItem.status_messages.length > 0
	);
</script>

<div
	class="request-card relative flex flex-col rounded-box transition-all duration-200"
	class:border-error={isFailedState}
	class:border={isFailedState}
	class:is-downloading={isActive && activeItem.status === 'downloading'}
>
	<a
		href={albumHref(item.musicbrainz_id)}
		class="absolute inset-0 z-0 rounded-box"
		aria-label="Open {item.album_title}"
	></a>

	<div class="relative z-10 flex items-center gap-3 sm:gap-4 p-3 sm:p-4 pointer-events-none">
		<div class="w-14 h-14 sm:w-18 sm:h-18 shrink-0 rounded-lg overflow-hidden relative">
			<AlbumImage
				mbid={item.musicbrainz_id}
				customUrl={item.cover_url ?? null}
				alt={item.album_title}
				size="sm"
				rounded="lg"
				className="w-full h-full"
			/>
			{#if isActive && activeItem.status === 'pending'}
				<div class="absolute inset-0 bg-base-300/60 flex items-center justify-center">
					<Loader class="h-5 w-5 text-warning animate-spin" />
				</div>
			{/if}
		</div>

		<div class="flex-1 min-w-0">
			<p class="font-semibold text-base-content text-sm sm:text-base line-clamp-1">
				{item.album_title}
			</p>
			{#if artistMbid}
				<a
					href={artistHref(artistMbid)}
					class="text-xs sm:text-sm text-base-content/70 hover:text-primary transition-colors line-clamp-1 text-left pointer-events-auto"
				>
					{item.artist_name}
				</a>
			{:else}
				<p class="text-xs sm:text-sm text-base-content/70 line-clamp-1 text-left">
					{item.artist_name}
				</p>
			{/if}
			<div class="flex items-center gap-1.5 mt-0.5 flex-wrap">
				<span class="text-xs text-base-content/40">
					{formatRelativeTime(item.requested_at)}
				</span>
				{#if item.year}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-base-content/40">{item.year}</span>
				{/if}
				{#if isActive && activeItem.quality}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-primary/70 font-medium">{activeItem.quality}</span>
				{/if}
				{#if isActive && activeItem.protocol}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-base-content/40 capitalize">{activeItem.protocol}</span>
				{/if}
				{#if !isActive && historyItem.reviewed_by_name}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-base-content/40">
						{historyItem.status === 'rejected' ? 'Rejected' : 'Approved'} by {historyItem.reviewed_by_name}
					</span>
				{/if}
			</div>
		</div>

		<div class="flex flex-col items-end gap-1.5 shrink-0 pointer-events-auto">
			<span
				class="badge {statusConfig.badgeClass} badge-sm gap-1"
				aria-label="Status: {statusConfig.label}"
			>
				<statusConfig.icon class="h-3 w-3" />
				{statusConfig.label}
			</span>

			{#if !isActive && watchState}
				<a
					href="/requests?tab=wanted"
					class="badge badge-sm badge-outline gap-1 border-info/30 text-info/80 hover:bg-info/10"
					title={watchState === 'retrying'
						? 'Auto-retrying - see the Wanted tab'
						: 'The watcher keeps checking for this - see the Wanted tab'}
				>
					<Radar class="h-3 w-3" />
					{watchState === 'retrying' ? 'Still hunting' : 'On the watchlist'}
				</a>
			{/if}

			{#if hasProgress}
				<div
					class="flex flex-col items-end gap-1 w-28 sm:w-44"
					role="status"
					aria-label="Download progress"
				>
					<div class="flex items-center gap-2 w-full">
						<div class="progress-track flex-1">
							<div
								class="progress-fill"
								class:progress-fill-paused={activeItem.status === 'paused'}
								style="width: {activeItem.progress ?? 0}%"
							></div>
						</div>
						<span
							class="text-xs text-base-content/70 min-w-[3ch] text-right tabular-nums font-medium"
						>
							{activeItem.progress?.toFixed(0) ?? 0}%
						</span>
					</div>
					<div class="flex items-center gap-2 text-xs text-base-content/40">
						{#if activeItem.eta}
							<span>{formatEta(activeItem.eta)}</span>
						{/if}
						{#if activeItem.eta && activeItem.size && activeItem.size_remaining != null}
							<span class="text-base-content/20">•</span>
						{/if}
						{#if activeItem.size && activeItem.size_remaining != null}
							<span class="tabular-nums">
								{formatSize(activeItem.size - (activeItem.size_remaining ?? 0))}/{formatSize(
									activeItem.size
								)}
							</span>
						{/if}
					</div>
				</div>
			{:else if isActive && isFailedState && activeItem.error_message}
				<span class="text-xs text-error/80 max-w-44 text-right line-clamp-2"
					>{activeItem.error_message}</span
				>
			{:else if !isActive && historyItem.completed_at}
				<span class="text-xs text-base-content/40">{formatDate(historyItem.completed_at)}</span>
			{/if}

			<div class="flex gap-1 items-center">
				{#if isActive && hasStatusMessages}
					<button
						class="btn btn-xs btn-ghost btn-square"
						onclick={(e) => {
							e.stopPropagation();
							showStatusDetails = !showStatusDetails;
						}}
						title={showStatusDetails ? 'Hide details' : 'Show details'}
						aria-expanded={showStatusDetails}
					>
						<ChevronDown
							class="h-3.5 w-3.5 transition-transform duration-200 {showStatusDetails
								? 'rotate-180'
								: ''}"
						/>
					</button>
				{/if}
				{#if isActive}
					{#if confirmingCancel}
						<span class="text-xs text-base-content/60 mr-0.5" role="alert">Cancel?</span>
						<button class="btn btn-xs btn-error" onclick={handleCancelClick}>Yes</button>
						<button class="btn btn-xs btn-ghost" onclick={handleCancelNo}>No</button>
					{:else}
						<button
							class="btn btn-xs btn-ghost text-error/60 hover:text-error hover:bg-error/10"
							onclick={handleCancelClick}
							title="Cancel download"
						>
							<X class="h-3.5 w-3.5" />
						</button>
					{/if}
				{:else}
					{#if historyItem.status === 'failed' || historyItem.status === 'cancelled' || historyItem.status === 'incomplete'}
						<button
							class="btn btn-xs btn-primary btn-ghost"
							onclick={handleRetry}
							title="Retry request"
						>
							<RotateCcw class="h-3.5 w-3.5" />
						</button>
					{/if}
					{#if authStore.isAdmin && historyItem.can_reimport && historyItem.download_task_id}
						<button
							class="btn btn-xs btn-ghost"
							onclick={(e) => {
								e.stopPropagation();
								reimport.mutate(
									{
										id: historyItem.download_task_id!,
										release_group_mbid: historyItem.musicbrainz_id
									},
									{ onSuccess: () => onreimported?.() }
								);
							}}
							disabled={reimport.isPending}
							title="Already fixed this in slskd? Check the downloads mount again without re-searching."
							aria-label="Retry import from slskd"
						>
							<FileDown class="h-3.5 w-3.5" />
						</button>
					{/if}
					{#if authStore.isAdmin && historyItem.status === 'imported' && historyItem.in_library}
						<button
							class="btn btn-xs btn-ghost text-base-content/40 hover:text-error"
							onclick={handleRemoveFromLibrary}
							title="Remove from library"
						>
							<Trash2 class="h-3.5 w-3.5" />
						</button>
					{/if}
					<button
						class="btn btn-xs btn-ghost text-base-content/30 hover:text-base-content/70"
						onclick={handleClear}
						title="Clear from history"
					>
						<X class="h-3.5 w-3.5" />
					</button>
				{/if}
			</div>
		</div>
	</div>

	{#if showStatusDetails && hasStatusMessages}
		<div class="px-3 sm:px-4 pb-3 sm:pb-4 -mt-1 relative z-10">
			<div
				class="bg-base-100/50 border border-error/10 rounded-lg p-3 text-xs max-h-48 overflow-y-auto"
			>
				{#each activeItem.status_messages ?? [] as msg, i (`${msg.title}-${i}`)}
					{#if msg.title}
						<div class="font-medium text-base-content/70 mt-2 first:mt-0">{msg.title}</div>
					{/if}
					{#each msg.messages as message, j (`${message}-${j}`)}
						<div class="text-error/70 ml-3 mt-0.5">• {message}</div>
					{/each}
				{/each}
			</div>
		</div>
	{/if}
</div>

{#if showDeleteModal}
	<DeleteAlbumModal
		albumTitle={item.album_title}
		artistName={item.artist_name}
		musicbrainzId={item.musicbrainz_id}
		ondeleted={handleDeleted}
		onclose={() => {
			showDeleteModal = false;
		}}
	/>
{/if}

{#if showArtistRemovedModal}
	<ArtistRemovedModal
		artistName={removedArtistName}
		onclose={() => {
			showArtistRemovedModal = false;
		}}
	/>
{/if}

<style>
	.request-card {
		background: oklch(from var(--color-base-200) l c h / 1);
	}
	.request-card:hover {
		background: oklch(from var(--color-base-300) l c h / 1);
		box-shadow: 0 0 12px oklch(from var(--color-primary) l c h / 0.06);
	}
	.request-card.is-downloading {
		box-shadow: 0 0 16px oklch(from var(--color-info) l c h / 0.08);
	}

	.progress-track {
		height: 6px;
		border-radius: 3px;
		background: oklch(from var(--color-base-300) l c h / 1);
		overflow: hidden;
	}
	.progress-fill {
		height: 100%;
		border-radius: 3px;
		background: linear-gradient(
			90deg,
			oklch(from var(--color-info) l c h / 0.8),
			oklch(from var(--color-info) l c h / 1)
		);
		transition: width 0.6s ease-out;
		position: relative;
	}
	.progress-fill::after {
		content: '';
		position: absolute;
		inset: 0;
		background: linear-gradient(
			90deg,
			transparent 0%,
			rgba(255, 255, 255, 0.15) 50%,
			transparent 100%
		);
		animation: shimmer 2s ease-in-out infinite;
	}
	.progress-fill-paused {
		background: oklch(from var(--color-warning) l c h / 0.6);
	}
	.progress-fill-paused::after {
		animation: none;
	}

	@keyframes shimmer {
		0% {
			transform: translateX(-100%);
		}
		100% {
			transform: translateX(100%);
		}
	}
</style>
