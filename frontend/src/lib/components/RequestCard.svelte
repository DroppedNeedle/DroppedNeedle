<script lang="ts">
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { withBasePath } from '$lib/utils/basePath';
	import AlbumImage from './AlbumImage.svelte';
	import DeleteAlbumModal from './DeleteAlbumModal.svelte';
	import type { RequestItem, RequestKind } from '$lib/queries/requests/types';
	import { reimportDownload } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { createFromAlbumMutation } from '$lib/queries/requests/RequestMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import {
		ChevronDown,
		ChevronUp,
		X,
		RotateCcw,
		Trash2,
		Clock,
		Download,
		FileDown,
		Loader,
		CircleCheckBig,
		Disc3,
		CircleX,
		TriangleAlert,
		Ban,
		Pause,
		Music,
		Radar,
		Upload,
		Eye
	} from 'lucide-svelte';

	interface Props {
		item: RequestItem;
		mode: 'active' | 'history';
		// The linked download's own state on an active card (searching, awaiting
		// review, downloading...), which the request row alone cannot tell.
		liveStatus?: string;
		// still being worked on despite the terminal status: 'retrying' = auto-retry
		// ladder, 'watching' = wanted watcher. Renders a chip linking to the Wanted tab.
		watchState?: 'retrying' | 'watching';
		// Batch-cancel selection. Rendered only when `selectable` is set.
		selectable?: boolean;
		selected?: boolean;
		onselect?: (mbid: string, requestKind: RequestKind, selected: boolean) => void;
		oncancel?: (mbid: string, requestKind: RequestKind) => void;
		onretry?: (mbid: string, requestKind: RequestKind) => void;
		onclear?: (mbid: string, requestKind: RequestKind) => void;
		onremoved?: () => void;
		onreimported?: () => void;
	}

	let {
		item,
		mode,
		liveStatus,
		watchState,
		selectable = false,
		selected = false,
		onselect,
		oncancel,
		onretry,
		onclear,
		onremoved,
		onreimported
	}: Props = $props();

	const reimport = reimportDownload();
	const fromAlbum = createFromAlbumMutation();

	let confirmingCancel = $state(false);
	let showDeleteModal = $state(false);

	function formatRelativeTime(epochSeconds: number): string {
		const diffMs = Date.now() - epochSeconds * 1000;
		const diffMin = Math.floor(diffMs / 60000);
		if (diffMin < 1) return 'just now';
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHr = Math.floor(diffMin / 60);
		if (diffHr < 24) return `${diffHr}h ago`;
		const diffDays = Math.floor(diffHr / 24);
		if (diffDays < 7) return `${diffDays}d ago`;
		return new Date(epochSeconds * 1000).toLocaleDateString();
	}

	function formatDate(epochSeconds: number): string {
		return new Date(epochSeconds * 1000).toLocaleString(undefined, {
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

	function formatEta(epochSeconds: number): string {
		const diffMs = epochSeconds * 1000 - Date.now();
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

	const waitingNote = $derived.by((): string | null => {
		if (mode !== 'active') return null;
		switch (item.waiting_reason) {
			case 'no_download_source':
				return 'Waiting: no download source configured';
			case 'request_quota_full':
				return 'Waiting: request quota full';
			case 'storage_full':
				return 'Waiting: storage full';
			default:
				return null;
		}
	});
	const waiting = $derived(waitingNote !== null);
	const status = $derived(mode === 'active' && liveStatus ? liveStatus : item.status);

	const statusConfig = $derived.by((): StatusConfig => {
		if (waiting) {
			return { badgeClass: 'badge-warning', label: 'Waiting', icon: Clock };
		}
		switch (status) {
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
				return { badgeClass: 'badge-error', label: 'Client Unavailable', icon: TriangleAlert };
			case 'importFailed':
				return { badgeClass: 'badge-error', label: 'Import Failed', icon: CircleX };
			case 'importBlocked':
				return { badgeClass: 'badge-warning', label: 'Import Blocked', icon: TriangleAlert };
			case 'cancelling':
				return { badgeClass: 'badge-ghost', label: 'Cancelling', icon: Loader };
			case 'incomplete':
				return { badgeClass: 'badge-warning', label: 'Incomplete', icon: TriangleAlert };
			case 'imported':
				return { badgeClass: 'badge-success', label: 'Imported', icon: CircleCheckBig };
			case 'failed':
				return { badgeClass: 'badge-error', label: 'Failed', icon: CircleX };
			case 'cancelled':
				return { badgeClass: 'badge-ghost', label: 'Cancelled', icon: Ban };
			case 'awaiting_approval':
				return { badgeClass: 'badge-warning', label: 'Awaiting Approval', icon: Clock };
			case 'awaiting_review':
				return { badgeClass: 'badge-warning', label: 'Awaiting Review', icon: Eye };
			case 'rejected':
				return { badgeClass: 'badge-error', label: 'Rejected', icon: Ban };
			default:
				return { badgeClass: 'badge-ghost', label: status, icon: Music };
		}
	});

	let showStatusDetails = $state(false);
	const isFailedState = $derived(
		item.status === 'importFailed' ||
			item.status === 'importBlocked' ||
			item.status === 'failed' ||
			item.status === 'downloadClientUnavailable'
	);
	const requestKind = $derived<RequestKind>(item.request_kind === 'track' ? 'track' : 'album');
	const isTrackRequest = $derived(requestKind === 'track');
	const trackTitle = $derived(
		isTrackRequest ? (item.track_title ?? 'Unknown track') : item.album_title
	);
	const artworkMbid = $derived(
		isTrackRequest ? (item.track_release_group_mbid ?? '') : item.musicbrainz_id
	);
	const albumContextMbid = $derived(
		isTrackRequest ? item.track_release_group_mbid : item.musicbrainz_id
	);

	function handleCancelClick(e: Event) {
		e.stopPropagation();
		if (confirmingCancel) {
			oncancel?.(item.musicbrainz_id, requestKind);
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
		onretry?.(item.musicbrainz_id, requestKind);
	}

	function handleClear(e: Event) {
		e.stopPropagation();
		onclear?.(item.musicbrainz_id, requestKind);
	}

	function handleSelect(e: Event) {
		e.stopPropagation();
		onselect?.(item.musicbrainz_id, requestKind, !selected);
	}

	function handleRemoveFromLibrary(e: Event) {
		e.stopPropagation();
		showDeleteModal = true;
	}

	function handleDeleted() {
		showDeleteModal = false;
		onremoved?.();
	}

	const isActive = $derived(mode === 'active');
	const hasProgress = $derived(
		isActive &&
			!waiting &&
			(status === 'downloading' || status === 'importing' || status === 'paused')
	);
	const hasStatusMessages = $derived(
		isActive && item.status_messages && item.status_messages.length > 0
	);
</script>

<Card
	padding="none"
	class="relative transition-colors hover:bg-base-300 {isFailedState ? 'border border-error' : ''}"
>
	{#if albumContextMbid}
		<a
			href={albumHref(albumContextMbid)}
			class="absolute inset-0 z-0 rounded-box"
			aria-label={isTrackRequest
				? `Open album context for ${item.album_title}`
				: `Open ${item.album_title}`}
		></a>
	{/if}

	<div class="relative z-10 flex items-center gap-3 sm:gap-4 p-3 sm:p-4 pointer-events-none">
		{#if selectable}
			<input
				type="checkbox"
				class="checkbox checkbox-sm shrink-0 pointer-events-auto"
				checked={selected}
				onclick={handleSelect}
				aria-label="Select {trackTitle}"
			/>
		{/if}
		<div class="w-14 h-14 sm:w-18 sm:h-18 shrink-0 rounded-lg overflow-hidden relative">
			<AlbumImage
				mbid={artworkMbid}
				customUrl={item.cover_url ?? null}
				alt={item.album_title}
				size="sm"
				rounded="lg"
				className="w-full h-full"
			/>
			{#if isActive && status === 'pending' && !waiting}
				<div class="absolute inset-0 bg-base-300/60 flex items-center justify-center">
					<Loader class="h-5 w-5 text-warning animate-spin motion-keep" />
				</div>
			{/if}
		</div>
		<div class="min-w-0 flex-1">
			<p class="font-semibold text-base-content text-sm sm:text-base line-clamp-1">
				{trackTitle}
			</p>
			{#if isTrackRequest}
				<p class="text-xs sm:text-sm text-base-content/70 line-clamp-1 text-left">
					<span class="badge badge-ghost badge-xs align-middle">Track</span>
					<span class="ml-1">Album: {item.album_title}</span>
				</p>
			{/if}
			{#if item.artist_mbid}
				<a
					href={artistHref(item.artist_mbid)}
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
				{#if item.origin === 'follow'}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-base-content/40">Followed artist</span>
				{/if}
				{#if isActive && item.quality}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-primary/70 font-medium">{item.quality}</span>
				{/if}
				{#if isActive && item.protocol}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-base-content/40 capitalize">{item.protocol}</span>
				{/if}
				{#if item.requester_count > 0}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-base-content/40">
						+{item.requester_count} other{item.requester_count === 1 ? '' : 's'}
					</span>
				{/if}
				{#if !isActive && item.reviewed_by_name}
					<span class="text-base-content/20">•</span>
					<span class="text-xs text-base-content/40">
						{item.status === 'rejected' ? 'Rejected' : 'Approved'} by {item.reviewed_by_name}
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
					href={withBasePath('/requests?tab=wanted')}
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
								class:progress-fill-paused={status === 'paused'}
								style="width: {item.progress ?? 0}%"
							></div>
						</div>
						<span
							class="text-xs text-base-content/70 min-w-[3ch] text-right tabular-nums font-medium"
						>
							{item.progress?.toFixed(0) ?? 0}%
						</span>
					</div>
					<div class="flex items-center gap-2 text-xs text-base-content/40">
						{#if item.eta}
							<span>{formatEta(item.eta)}</span>
						{/if}
						{#if item.eta && item.size && item.size_remaining != null}
							<span class="text-base-content/20">•</span>
						{/if}
						{#if item.size && item.size_remaining != null}
							<span class="tabular-nums">
								{formatSize(item.size - (item.size_remaining ?? 0))}/{formatSize(item.size)}
							</span>
						{/if}
					</div>
				</div>
			{:else if waitingNote}
				<span class="text-xs text-base-content/70 max-w-44 text-right">
					{waitingNote}
				</span>
			{:else if isActive && isFailedState && item.error_message}
				<span class="text-xs text-error/80 max-w-44 text-right line-clamp-2"
					>{item.error_message}</span
				>
			{:else if !isActive && item.album_version_only}
				<span class="text-xs text-base-content/70 max-w-44 text-right">
					Only the album version was found
				</span>
			{:else if !isActive && item.completed_at}
				<span class="text-xs text-base-content/40">{formatDate(item.completed_at)}</span>
			{/if}

			<div class="flex gap-1 items-center">
				{#if isActive && hasStatusMessages}
					<IconButton
						size="xs"
						shape="square"
						icon={showStatusDetails ? ChevronUp : ChevronDown}
						label={showStatusDetails ? 'Hide details' : 'Show details'}
						onclick={(e: MouseEvent) => {
							e.stopPropagation();
							showStatusDetails = !showStatusDetails;
						}}
						aria-expanded={showStatusDetails}
					/>
				{/if}
				{#if isActive}
					{#if confirmingCancel}
						<span class="text-xs text-base-content/60 mr-0.5" role="alert">Cancel?</span>
						<Button variant="danger" size="xs" onclick={handleCancelClick}>Yes</Button>
						<Button variant="ghost" size="xs" onclick={handleCancelNo}>No</Button>
					{:else}
						<Button
							variant="danger-ghost"
							size="xs"
							icon={X}
							onclick={handleCancelClick}
							title="Cancel download"
							aria-label="Cancel download"
						/>
					{/if}
				{:else}
					{#if item.album_version_only && onretry}
						<Button
							variant="ghost"
							size="xs"
							icon={Disc3}
							onclick={(e: MouseEvent) => {
								e.stopPropagation();
								fromAlbum.mutate(item.musicbrainz_id);
							}}
							disabled={fromAlbum.isPending}
						>
							Get it from the album
						</Button>
					{/if}
					{#if item.status === 'failed' || item.status === 'cancelled' || item.status === 'incomplete'}
						<Button
							variant="ghost"
							size="xs"
							icon={RotateCcw}
							onclick={handleRetry}
							title="Retry request"
							aria-label="Retry request"
						/>
					{/if}
					{#if authStore.isAdmin && item.can_reimport && item.task_id}
						<Button
							variant="ghost"
							size="xs"
							icon={FileDown}
							onclick={(e: MouseEvent) => {
								e.stopPropagation();
								reimport.mutate(
									{
										id: item.task_id!,
										release_group_mbid:
											item.track_release_group_mbid ?? (isTrackRequest ? null : item.musicbrainz_id)
									},
									{ onSuccess: () => onreimported?.() }
								);
							}}
							disabled={reimport.isPending}
							title="Already fixed this in slskd? Check the downloads mount again without re-searching."
							aria-label="Retry import from slskd"
						/>
					{/if}
					{#if authStore.isAdmin && !isTrackRequest && item.status === 'imported' && item.in_library !== false}
						<Button
							variant="ghost"
							size="xs"
							icon={Trash2}
							onclick={handleRemoveFromLibrary}
							title="Remove from library"
							aria-label="Remove from library"
						/>
					{/if}
					<Button
						variant="ghost"
						size="xs"
						icon={X}
						onclick={handleClear}
						title="Clear from history"
						aria-label="Clear from history"
					/>
				{/if}
			</div>
		</div>
	</div>

	{#if showStatusDetails && hasStatusMessages}
		<div class="px-3 sm:px-4 pb-3 sm:pb-4 -mt-3 relative z-10">
			<div
				class="bg-base-100/50 border border-error/10 rounded-lg p-3 text-xs max-h-48 overflow-y-auto"
			>
				{#each item.status_messages ?? [] as msg, i (`${msg.title}-${i}`)}
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
</Card>

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

<style>
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
