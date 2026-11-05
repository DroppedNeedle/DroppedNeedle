<script lang="ts">
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { withBasePath } from '$lib/utils/basePath';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import DeleteAlbumModal from '$lib/features/album/components/DeleteAlbumModal.svelte';
	import type { RequestItem, RequestKind } from '$lib/features/requests/queries/types';
	import { liveRequestStatus } from '$lib/features/requests/queries/liveStatus';
	import { createReimportDownloadMutation } from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import { createFromAlbumMutation } from '$lib/features/requests/queries/RequestMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import RequestRow from './RequestRow.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import {
		ChevronDown,
		ChevronUp,
		Info,
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
		Music,
		Radar,
		Upload,
		Eye
	} from 'lucide-svelte';

	interface Props {
		item: RequestItem;
		mode: 'active' | 'history';
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

	const reimport = createReimportDownloadMutation();
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
		tone: 'warning' | 'info' | 'neutral' | 'success' | 'error';
		label: string;
		icon: typeof Clock;
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
			case 'waiting_to_start':
				return 'Waiting to start';
			default:
				return null;
		}
	});
	const waiting = $derived(waitingNote !== null);
	// An active card shows the linked download's own state (searching,
	// awaiting review...), which the request row alone cannot tell.
	const status = $derived(mode === 'active' ? liveRequestStatus(item) : item.status);

	const statusConfig = $derived.by((): StatusConfig => {
		if (waiting) {
			return { tone: 'warning', label: 'Waiting', icon: Clock };
		}
		switch (status) {
			case 'pending':
				return {
					tone: 'warning',
					label: 'Searching',
					icon: Clock
				};
			case 'queued':
				return { tone: 'warning', label: 'Queued', icon: Clock };
			case 'downloading':
				return {
					tone: 'info',
					label: 'Downloading',
					icon: Download
				};
			case 'importing':
				return { tone: 'info', label: 'Importing', icon: Upload };
			case 'cancelling':
				return { tone: 'neutral', label: 'Cancelling', icon: Loader };
			case 'incomplete':
				return { tone: 'warning', label: 'Incomplete', icon: TriangleAlert };
			case 'imported':
				return { tone: 'success', label: 'Imported', icon: CircleCheckBig };
			case 'failed':
				return { tone: 'error', label: 'Failed', icon: CircleX };
			case 'cancelled':
				return { tone: 'neutral', label: 'Cancelled', icon: Ban };
			case 'awaiting_approval':
				return { tone: 'warning', label: 'Awaiting Approval', icon: Clock };
			case 'awaiting_review':
				return { tone: 'warning', label: 'Awaiting Review', icon: Eye };
			case 'rejected':
				return { tone: 'error', label: 'Rejected', icon: Ban };
			default:
				return { tone: 'neutral', label: status, icon: Music };
		}
	});

	let showStatusDetails = $state(false);
	const isFailedState = $derived(item.status === 'failed');
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
		isActive && !waiting && (status === 'downloading' || status === 'importing')
	);
	const hasStatusMessages = $derived(
		isActive && item.status_messages && item.status_messages.length > 0
	);
</script>

{#snippet cover()}
	<div class="relative size-full">
		<AlbumImage
			mbid={artworkMbid}
			customUrl={item.cover_url ?? null}
			alt={item.album_title}
			size="sm"
			rounded="none"
			className="w-full h-full"
		/>
		{#if isActive && status === 'pending' && !waiting}
			<div class="absolute inset-0 grid place-items-center bg-base-300/60">
				<Loader class="size-6 text-warning animate-spin motion-keep" aria-hidden="true" />
			</div>
		{/if}
	</div>
{/snippet}

{#snippet lead()}
	<input
		type="checkbox"
		class="checkbox"
		checked={selected}
		onclick={handleSelect}
		aria-label="Select {trackTitle}"
	/>
{/snippet}

{#snippet details()}
	<Card variant="torn" padding="none" class="rq-slip">
		<div class="rq-details">
			<section class="rq-details__part">
				<h3 class="rq-details__head">Request details</h3>
				<dl class="rq-details__list">
					<dt>Type</dt>
					<dd>{isTrackRequest ? 'Track' : 'Album'}</dd>
					<dt>Requested</dt>
					<dd>{formatDate(item.requested_at)}</dd>
					{#if item.quality}
						<dt>Quality</dt>
						<dd>{item.quality}</dd>
					{/if}
					{#if item.protocol}
						<dt>Protocol</dt>
						<dd class="capitalize">{item.protocol}</dd>
					{/if}
					<dt>Status</dt>
					<dd>{statusConfig.label}</dd>
				</dl>
			</section>
			<section class="rq-details__part rq-details__part--messages">
				<h3 class="rq-details__head">Messages</h3>
				<ul class="rq-details__messages">
					{#each item.status_messages ?? [] as msg, i (`${msg.title}-${i}`)}
						<li class="rq-details__group">
							<Info class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
							<div class="min-w-0">
								{#if msg.title}
									<p class="rq-details__title">{msg.title}</p>
								{/if}
								{#each msg.messages as message, j (`${message}-${j}`)}
									<p class="rq-details__line">{message}</p>
								{/each}
							</div>
						</li>
					{/each}
				</ul>
			</section>
		</div>
	</Card>
{/snippet}

{#snippet aside()}
	{#if !isActive && watchState}
		<a
			href={withBasePath('/requests?tab=wanted')}
			class="rq-pill rq-pill--outline rq-pill--info"
			title={watchState === 'retrying'
				? 'Auto-retrying - see the Wanted tab'
				: 'The watcher keeps checking for this - see the Wanted tab'}
		>
			<Radar aria-hidden="true" />
			{watchState === 'retrying' ? 'Still hunting' : 'On the watchlist'}
		</a>
	{/if}

	{#if hasProgress}
		<div class="rq-progress" role="status" aria-label="Download progress">
			<div class="rq-progress__bar">
				<div class="rq-progress__track">
					<div class="rq-progress__fill" style:width="{item.progress ?? 0}%"></div>
				</div>
				<span class="rq-progress__pct">{item.progress?.toFixed(0) ?? 0}%</span>
			</div>
			<div class="rq-progress__meta">
				{#if item.eta}
					<span>{formatEta(item.eta)}</span>
				{/if}
				{#if item.size && item.size_remaining != null}
					<span>
						{formatSize(item.size - (item.size_remaining ?? 0))}/{formatSize(item.size)}
					</span>
				{/if}
			</div>
		</div>
	{:else if waitingNote}
		<span class="rq-note">{waitingNote}</span>
	{:else if isActive && isFailedState && item.error_message}
		<span class="rq-note rq-note--error line-clamp-2">{item.error_message}</span>
	{:else if !isActive && item.album_version_only}
		<span class="rq-note">Only the album version was found</span>
	{:else if !isActive && item.completed_at}
		<span class="rq-date">{formatDate(item.completed_at)}</span>
	{/if}

	<div class="rq-actions">
		{#if isActive && hasStatusMessages}
			<Button
				variant="outline"
				size="display-sm"
				icon={showStatusDetails ? ChevronUp : ChevronDown}
				iconPosition="end"
				aria-expanded={showStatusDetails}
				onclick={(e: MouseEvent) => {
					e.stopPropagation();
					showStatusDetails = !showStatusDetails;
				}}
			>
				{showStatusDetails ? 'Hide details' : 'Show details'}
			</Button>
		{/if}
		{#if isActive}
			{#if confirmingCancel}
				<span class="rq-confirm" role="alert">Cancel?</span>
				<Button variant="danger" size="display-sm" onclick={handleCancelClick}>Yes</Button>
				<Button variant="outline" size="display-sm" onclick={handleCancelNo}>No</Button>
			{:else}
				<Button variant="outline" size="display-sm" icon={X} onclick={handleCancelClick}>
					Cancel download
				</Button>
			{/if}
		{:else}
			{#if item.album_version_only && onretry}
				<Button
					variant="outline"
					size="display-sm"
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
				<IconButton
					variant="outline"
					size="display-sm"
					shape="square"
					icon={RotateCcw}
					label="Retry request"
					onclick={handleRetry}
				/>
			{/if}
			{#if authStore.isAdmin && item.can_reimport && item.task_id}
				<span class="rq-square">
					<Button
						variant="outline"
						size="display-sm"
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
						title="Already fixed this in the download client? Check the downloads mount again without re-searching."
						aria-label="Retry import"
					/>
				</span>
			{/if}
			{#if authStore.isAdmin && !isTrackRequest && item.status === 'imported' && item.in_library !== false}
				<IconButton
					variant="outline"
					size="display-sm"
					shape="square"
					icon={Trash2}
					label="Remove from library"
					onclick={handleRemoveFromLibrary}
				/>
			{/if}
			<IconButton
				variant="outline"
				size="display-sm"
				shape="square"
				icon={X}
				label="Clear from history"
				onclick={handleClear}
			/>
		{/if}
	</div>
{/snippet}

<RequestRow
	tone={isFailedState ? 'error' : waiting ? 'warning' : null}
	href={albumContextMbid ? albumHref(albumContextMbid) : undefined}
	linkLabel={isTrackRequest
		? `Open album context for ${item.album_title}`
		: `Open ${item.album_title}`}
	lead={selectable ? lead : undefined}
	{cover}
	{aside}
	below={showStatusDetails && hasStatusMessages ? details : undefined}
>
	<div class="rq-head">
		<p class="rq-title">{trackTitle}</p>
		<span
			class="rq-pill rq-pill--{statusConfig.tone}"
			class:rq-pill--outline={!isActive}
			aria-label="Status: {statusConfig.label}"
		>
			<statusConfig.icon aria-hidden="true" />
			{statusConfig.label}
		</span>
	</div>
	{#if isTrackRequest}
		<p class="rq-album">
			<span class="rq-pill rq-pill--neutral rq-pill--outline rq-pill--tiny">Track</span>
			<span class="truncate">Album: {item.album_title}</span>
		</p>
	{/if}
	{#if item.artist_mbid}
		<a href={artistHref(item.artist_mbid)} class="rq-artist">{item.artist_name}</a>
	{:else}
		<p class="rq-artist">{item.artist_name}</p>
	{/if}
	<p class="rq-meta mt-2">
		<span>Asked {formatRelativeTime(item.requested_at)}</span>
		{#if item.year}
			<span>Year {item.year}</span>
		{/if}
		{#if item.origin === 'follow'}
			<span>Followed artist</span>
		{/if}
		{#if isActive && item.quality}
			<span class="font-semibold text-title">{item.quality}</span>
		{/if}
		{#if isActive && item.protocol}
			<span>Protocol {item.protocol}</span>
		{/if}
		{#if item.requester_count > 0}
			<span>+{item.requester_count} other{item.requester_count === 1 ? '' : 's'}</span>
		{/if}
		{#if !isActive && item.reviewed_by_name}
			<span>
				{item.status === 'rejected' ? 'Rejected' : 'Approved'} by {item.reviewed_by_name}
			</span>
		{/if}
	</p>
</RequestRow>

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
	.rq-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.375rem 1.25rem;
		margin-bottom: 0.25rem;
	}
	.rq-album {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 0;
		margin-bottom: 0.125rem;
		font-size: 0.9375rem;
		color: var(--dn-muted);
	}
	.rq-album .rq-pill {
		height: 1.375rem;
		padding-inline: 0.5rem;
		font-size: 0.6875rem;
	}

	.rq-actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: 0.625rem;
	}
	/* an icon button that keeps a tooltip longer than its name */
	.rq-square :global(.btn) {
		width: var(--size);
		padding: 0;
	}
	.rq-confirm {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.rq-note {
		max-width: 16rem;
		font-size: 0.875rem;
		text-align: right;
		color: var(--dn-muted);
	}
	.rq-note--error {
		color: var(--color-error);
	}
	.rq-date {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
	}

	.rq-progress {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 0.375rem;
		width: 15rem;
	}
	.rq-progress__bar {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		width: 100%;
	}
	.rq-progress__track {
		flex: 1;
		height: 0.5rem;
		overflow: hidden;
		border-radius: 9999px;
		background-color: var(--color-base-300);
		box-shadow: inset 0 0 0 1px var(--dn-line);
	}
	.rq-progress__fill {
		height: 100%;
		border-radius: inherit;
		background-color: var(--dn-title);
		transition: width 0.6s ease-out;
	}
	.rq-progress__pct,
	.rq-progress__meta {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.06em;
		font-variant-numeric: tabular-nums;
	}
	.rq-progress__pct {
		min-width: 3.5ch;
		text-align: right;
	}
	.rq-progress__meta {
		display: flex;
		gap: 0.75rem;
		font-weight: 500;
		color: var(--dn-muted);
	}

	/* the details: a torn paper slip, the facts left and the messages right;
	   on the light theme's paper-white card the slip takes the page's tone */
	:global([data-theme='light'] .rq-slip) {
		background-color: var(--color-base-300);
	}
	.rq-details {
		display: grid;
		grid-template-columns: minmax(0, 2fr) minmax(0, 3fr);
		padding: 1.75rem 1.75rem 1.5rem;
	}
	.rq-details__part--messages {
		margin-left: 1.75rem;
		padding-left: 1.75rem;
		border-left: 1.5px solid color-mix(in oklab, var(--dn-paper-content) 35%, transparent);
	}
	.rq-details__head {
		margin-bottom: 0.75rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.rq-details__list {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: 0.3rem 1.5rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
	}
	.rq-details__list dt {
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.rq-details__list dd {
		overflow-wrap: anywhere;
	}
	.rq-details__messages {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-height: 12rem;
		overflow-y: auto;
	}
	.rq-details__group {
		display: flex;
		gap: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
	}
	.rq-details__title {
		font-weight: 700;
	}
	.rq-details__line {
		color: var(--dn-muted);
		overflow-wrap: anywhere;
	}

	@media (max-width: 767px) {
		.rq-head {
			gap: 0.375rem 0.75rem;
		}
		.rq-progress {
			flex: 1 1 100%;
			width: auto;
		}
		.rq-note {
			flex: 1 1 100%;
			max-width: none;
			text-align: left;
		}
		.rq-actions :global(.btn) {
			--size: 2.25rem;
			--fontsize: 0.9375rem;
			--btn-p: 0.75rem;
		}
		.rq-details {
			grid-template-columns: minmax(0, 1fr);
			gap: 1.25rem;
			padding: 1.5rem 1.125rem 1.25rem;
		}
		.rq-details__part--messages {
			margin-left: 0;
			padding-top: 1.25rem;
			padding-left: 0;
			border-top: 1.5px solid color-mix(in oklab, var(--dn-paper-content) 35%, transparent);
			border-left: 0;
		}
	}
</style>
