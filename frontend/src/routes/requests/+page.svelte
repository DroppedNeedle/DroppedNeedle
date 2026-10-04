<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { SvelteMap, SvelteSet } from 'svelte/reactivity';
	import { page } from '$app/state';
	import { fade, fly } from 'svelte/transition';
	import RequestCard from '$lib/components/RequestCard.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import Toast from '$lib/components/Toast.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import type { RequestItem } from '$lib/queries/requests/types';
	import type { RequestKind } from '$lib/constants';
	import {
		TriangleAlert,
		CircleCheck,
		Clock,
		Download,
		History,
		Radar,
		Search,
		ShieldCheck,
		Check,
		X,
		Heart,
		Sparkles,
		CloudDownload,
		TrendingUp
	} from 'lucide-svelte';
	import WantedWatchCard from '$lib/components/WantedWatchCard.svelte';
	import WantedRetryingCard from '$lib/components/WantedRetryingCard.svelte';
	import { getWantedWatchesQuery } from '$lib/queries/wanted/WantedQuery.svelte';
	import {
		createStopWatchMutation,
		createResumeWatchMutation,
		createMarkWantedSeenMutation
	} from '$lib/queries/wanted/WantedMutations.svelte';
	import type { WantedWatchItem } from '$lib/queries/wanted/types';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import {
		getActiveRequestCountQuery,
		getActiveRequestsQuery,
		getApprovalsQuery,
		getRequestHistoryQuery
	} from '$lib/queries/requests/RequestQueries.svelte';
	import {
		createApproveRequestMutation,
		createBatchCancelRequestsMutation,
		createCancelRequestMutation,
		createClearHistoryMutation,
		createRejectRequestMutation,
		createRetryRequestMutation
	} from '$lib/queries/requests/RequestMutations.svelte';
	import {
		getAutoDownloadApprovalsQuery,
		getAutoDownloadApprovalBatchesQuery
	} from '$lib/queries/following/AdminApprovalsQueries.svelte';
	import {
		createApproveAutoDownloadMutation,
		createRejectAutoDownloadMutation,
		createApproveAutoDownloadBatchMutation,
		createRejectAutoDownloadBatchMutation
	} from '$lib/queries/following/AdminApprovalsMutations.svelte';
	import { getPersonalMixApprovalsQuery } from '$lib/queries/scrobble-preferences/PersonalMixApprovalsQuery.svelte';
	import {
		createApprovePersonalMixMutation,
		createRejectPersonalMixMutation
	} from '$lib/queries/scrobble-preferences/ScrobblePreferencesMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getCutoffUnmetQuery,
		requestUpgradeAlbum
	} from '$lib/queries/downloads/UpgradeQueries.svelte';
	import { QUALITY_TIERS } from '$lib/components/settings/qualityTiers';

	type RequestsTab = 'active' | 'history' | 'wanted' | 'approvals' | 'auto-download' | 'upgrades';
	let activeTab = $state<RequestsTab>('active');

	// Wanted watches (availability re-search). TanStack per current convention;
	// fetched on the Wanted tab AND on History (whose failed rows show a
	// still-hunting/watchlist chip), refreshed by the wanted_* SSE events.
	const wantedQuery = getWantedWatchesQuery(
		() => activeTab === 'wanted' || activeTab === 'history'
	);
	const wantedItems = $derived(wantedQuery.data?.items ?? []);
	const wantedRetrying = $derived(wantedQuery.data?.retrying ?? []);
	const wantedActiveCount = $derived(
		wantedItems.filter((i) => i.state === 'watching').length + wantedRetrying.length
	);
	// mbid (lowercased) -> chip state for History rows: a terminal-looking request
	// that's actually still being worked on must never read as dead
	const wantedStates = $derived.by(() => {
		const map = new SvelteMap<string, 'retrying' | 'watching'>();
		for (const entry of wantedRetrying) {
			map.set(entry.musicbrainz_id.toLowerCase(), 'retrying');
		}
		for (const watch of wantedItems) {
			if (watch.state === 'watching') {
				map.set(watch.musicbrainz_id.toLowerCase(), 'watching');
			}
		}
		return map;
	});
	const stopWatch = createStopWatchMutation();
	const resumeWatch = createResumeWatchMutation();
	const markWantedSeen = createMarkWantedSeenMutation();
	const wantedBusy = $derived(stopWatch.isPending || resumeWatch.isPending);

	function handleWantedStop(item: WantedWatchItem) {
		stopWatch.mutate({ mbid: item.musicbrainz_id, albumTitle: item.album_title });
	}

	function handleWantedResume(item: WantedWatchItem) {
		resumeWatch.mutate({ mbid: item.musicbrainz_id, albumTitle: item.album_title });
	}

	function handleWantedSeen(item: WantedWatchItem) {
		markWantedSeen.mutate({ mbid: item.musicbrainz_id, albumTitle: item.album_title });
	}

	// Cutoff-unmet worklist (admin/trusted curators, CollectionManagement D7/D18).
	const cutoffUnmetQuery = getCutoffUnmetQuery(
		() => authStore.isTrusted && activeTab === 'upgrades'
	);
	const upgradeItems = $derived(cutoffUnmetQuery.data?.items ?? []);
	const upgradeAlbum = requestUpgradeAlbum();
	// albums this visit already queued an upgrade for (button flips to "Queued")
	let upgradeQueued = $state<Set<string>>(new Set());

	function tierLabel(key: string): string {
		return QUALITY_TIERS.find((t) => t.key === key)?.full ?? key;
	}

	async function handleUpgrade(item: (typeof upgradeItems)[number]) {
		try {
			const result = await upgradeAlbum.mutateAsync({
				release_group_mbid: item.release_group_mbid,
				artist_name: item.artist_name ?? 'Unknown',
				album_title: item.album_title ?? 'Unknown',
				year: item.year,
				artist_mbid: item.artist_mbid
			});
			if (result.status === 'queued') {
				upgradeQueued = new Set([...upgradeQueued, item.release_group_mbid]);
				showToast(`Looking for a better copy of ${item.album_title ?? 'this album'}`);
			} else {
				showToast('Already at or above the cutoff', 'info');
			}
		} catch (e) {
			showToast(e instanceof Error ? e.message : "Couldn't start that upgrade", 'error');
		}
	}

	// Auto-download standing approvals (TanStack); only fetched for admins on this tab.
	const autoApprovalsQuery = getAutoDownloadApprovalsQuery(
		() => authStore.isAdmin && activeTab === 'auto-download'
	);
	const autoApprovals = $derived(autoApprovalsQuery.data?.items ?? []);
	const approveAuto = createApproveAutoDownloadMutation();
	const rejectAuto = createRejectAutoDownloadMutation();

	// Bulk "Lidarr Import" approval batches share the auto-download tab (LidarrImport D3).
	const batchApprovalsQuery = getAutoDownloadApprovalBatchesQuery(
		() => authStore.isAdmin && activeTab === 'auto-download'
	);
	const batchApprovals = $derived(batchApprovalsQuery.data?.batches ?? []);
	const approveBatch = createApproveAutoDownloadBatchMutation();
	const rejectBatch = createRejectAutoDownloadBatchMutation();

	// Weekly Mix auto-request standing approvals share the auto-download tab.
	const mixApprovalsQuery = getPersonalMixApprovalsQuery(
		() => authStore.isAdmin && activeTab === 'auto-download'
	);
	const mixApprovals = $derived(mixApprovalsQuery.data?.items ?? []);
	const autoApprovalCount = $derived(
		(autoApprovalsQuery.data?.count ?? 0) +
			(batchApprovalsQuery.data?.count ?? 0) +
			(mixApprovalsQuery.data?.count ?? 0)
	);
	const approveMix = createApprovePersonalMixMutation();
	const rejectMix = createRejectPersonalMixMutation();

	function approvalTimeAgo(epochSeconds: number): string {
		const diff = Date.now() / 1000 - epochSeconds;
		if (diff < 60) return 'just now';
		if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
		if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
		return `${Math.floor(diff / 86400)}d ago`;
	}

	// Active, history, and approvals all read through TanStack Query: the
	// request mutations invalidate the requests prefix, so every tab refreshes
	// itself after a row action with no manual reloads.
	const activeListQuery = getActiveRequestsQuery(() => activeTab === 'active');
	const activeItems = $derived(activeListQuery.data?.items ?? []);
	const activeCount = $derived(activeListQuery.data?.count ?? activeItems.length);
	const activeLoading = $derived(activeListQuery.isPending);
	const activeError = $derived(
		activeListQuery.isError ? "Couldn't load active requests" : null
	);
	const isPolling = $derived(activeListQuery.isFetching);

	let historyPage = $state(1);
	const historyPageSize = 20;
	let historyFilter = $state<string | undefined>(undefined);
	let historySort = $state<string | undefined>(undefined);
	const historyQuery = getRequestHistoryQuery(
		() => ({
			page: historyPage,
			pageSize: historyPageSize,
			status: historyFilter,
			sort: historySort
		}),
		() => activeTab === 'history'
	);
	const historyItems = $derived<RequestItem[]>(historyQuery.data?.items ?? []);
	const historyTotal = $derived(historyQuery.data?.total ?? 0);
	const historyTotalPages = $derived(historyQuery.data?.total_pages ?? 1);
	const historyLoading = $derived(historyQuery.isPending);
	const historyError = $derived(historyQuery.isError ? "Couldn't load request history" : null);

	// Approvals stay fetched for every admin visit so the tab badge is live
	// even before the tab opens.
	const approvalsQuery = getApprovalsQuery(() => authStore.isAdmin);
	const approvalItems = $derived<RequestItem[]>(approvalsQuery.data?.items ?? []);
	const approvalCount = $derived(approvalsQuery.data?.count ?? approvalItems.length);
	const approvalLoading = $derived(approvalsQuery.isPending);
	const approvalError = $derived(
		approvalsQuery.isError ? "Couldn't load pending approvals" : null
	);

	let toastShow = $state(false);
	let toastMessage = $state('');
	let toastType = $state<'success' | 'error' | 'info'>('success');

	function requestKey(mbid: string, requestKind: RequestKind): string {
		return `${requestKind}:${mbid}`;
	}

	function itemKind(item: Pick<RequestItem, 'request_kind'>): RequestKind {
		return item.request_kind === 'track' ? 'track' : 'album';
	}

	function itemRequestKey(item: Pick<RequestItem, 'musicbrainz_id' | 'request_kind'>): string {
		return requestKey(item.musicbrainz_id, itemKind(item));
	}

	// R10: the tab badge reads the cheap active-count endpoint and falls back
	// to the list count while it loads.
	const activeCountQuery = getActiveRequestCountQuery(() => !!authStore.user?.id);
	const activeBadgeCount = $derived(activeCountQuery.data?.count ?? activeCount);

	// R10 batch-cancel selection on the Active tab.
	let selectedKeys = $state<Set<string>>(new Set());
	const selectedList = $derived(activeItems.filter((i) => selectedKeys.has(itemRequestKey(i))));

	function handleSelect(mbid: string, requestKind: RequestKind, selected: boolean) {
		const next = new SvelteSet(selectedKeys);
		if (selected) next.add(requestKey(mbid, requestKind));
		else next.delete(requestKey(mbid, requestKind));
		selectedKeys = next;
	}

	const cancelRequestMutation = createCancelRequestMutation();
	const retryRequestMutation = createRetryRequestMutation();
	const clearHistoryMutation = createClearHistoryMutation();
	const approveRequestMutation = createApproveRequestMutation();
	const rejectRequestMutation = createRejectRequestMutation();
	const batchCancelMutation = createBatchCancelRequestsMutation();

	async function handleBatchCancel() {
		const albumMbids = selectedList
			.filter((i) => itemKind(i) === 'album')
			.map((i) => i.musicbrainz_id);
		const trackMbids = selectedList
			.filter((i) => itemKind(i) === 'track')
			.map((i) => i.musicbrainz_id);
		try {
			if (albumMbids.length > 0) {
				await batchCancelMutation.mutateAsync({ mbids: albumMbids, kind: 'album' });
			}
			if (trackMbids.length > 0) {
				await batchCancelMutation.mutateAsync({ mbids: trackMbids, kind: 'track' });
			}
			selectedKeys = new Set();
		} catch {
			// the mutation already toasted the failure
		}
	}
	const downloadingCount = $derived(activeItems.filter((i) => i.status === 'downloading').length);
	const pendingCount = $derived(
		activeItems.filter((i) => i.status === 'pending' || i.status === 'queued').length
	);

	function showToast(message: string, type: 'success' | 'error' | 'info' = 'success') {
		toastMessage = message;
		toastType = type;
		toastShow = true;
	}

	// Drop batch-cancel selections for rows that settled since the last poll.
	$effect(() => {
		const live = new Set(activeItems.map(itemRequestKey));
		if (selectedKeys.size > 0 && ![...selectedKeys].every((key) => live.has(key))) {
			selectedKeys = new Set([...selectedKeys].filter((key) => live.has(key)));
		}
	});

	async function handleApprove(mbid: string, requestKind: RequestKind) {
		try {
			await approveRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	async function handleReject(mbid: string, requestKind: RequestKind) {
		try {
			await rejectRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	function switchTab(tab: RequestsTab) {
		activeTab = tab;
	}

	async function handleCancel(mbid: string, requestKind: RequestKind) {
		try {
			await cancelRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	async function handleRetry(mbid: string, requestKind: RequestKind) {
		try {
			await retryRequestMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	async function handleClear(mbid: string, requestKind: RequestKind) {
		try {
			await clearHistoryMutation.mutateAsync({ mbid, kind: requestKind });
		} catch {
			// the mutation already toasted the failure
		}
	}

	function handleRemoved() {
		void historyQuery.refetch();
	}

	function handleHistoryPageChange(page: number) {
		historyPage = page;
	}

	function handleFilterChange(e: Event) {
		const value = (e.target as HTMLSelectElement).value;
		historyFilter = value || undefined;
		historyPage = 1;
	}

	function handleSortChange(e: Event) {
		const value = (e.target as HTMLSelectElement).value;
		historySort = value || undefined;
		historyPage = 1;
	}

	onMount(() => {
		const tabParam = page.url.searchParams.get('tab');
		if (tabParam === 'approvals' && authStore.isAdmin) {
			switchTab('approvals');
		} else if (tabParam === 'wanted') {
			switchTab('wanted');
		}
	});

	// sidebar Requests/Approvals links navigate without remounting, so onMount can't switch tabs on those clicks; skip first run (onMount sets initial tab) and untrack activeTab so in-page buttons aren't overridden
	let tabSyncReady = false;
	$effect(() => {
		const tabParam = page.url.searchParams.get('tab');
		if (!tabSyncReady) {
			tabSyncReady = true;
			return;
		}
		const target: 'active' | 'history' | 'wanted' | 'approvals' =
			tabParam === 'approvals' && authStore.isAdmin
				? 'approvals'
				: tabParam === 'history'
					? 'history'
					: tabParam === 'wanted'
						? 'wanted'
						: 'active';
		if (untrack(() => activeTab) !== target) {
			switchTab(target);
		}
	});
</script>

<div class="container mx-auto px-4 sm:px-6 lg:px-8 py-6 sm:py-8">
	<div class="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-3 mb-6">
		<div>
			<h1 class="text-2xl sm:text-3xl font-bold text-base-content">Requests</h1>
			<p class="text-base-content/50 text-sm mt-0.5">
				What you've asked for and where each one stands
			</p>
		</div>
		{#if activeCount > 0}
			<div class="flex items-center gap-3 text-xs text-base-content/50">
				{#if downloadingCount > 0}
					<span class="flex items-center gap-1.5">
						<Download class="h-3.5 w-3.5 text-info" />
						{downloadingCount} downloading
					</span>
				{/if}
				{#if pendingCount > 0}
					<span class="flex items-center gap-1.5">
						<Search class="h-3.5 w-3.5 text-warning" />
						{pendingCount} searching
					</span>
				{/if}
			</div>
		{/if}
	</div>

	<div
		class="flex items-center gap-1 mb-6 border-b border-base-content/5 pb-px overflow-x-auto"
		role="tablist"
	>
		<button
			role="tab"
			class="tab-btn"
			class:tab-btn-active={activeTab === 'active'}
			aria-selected={activeTab === 'active'}
			onclick={() => switchTab('active')}
		>
			<Download class="h-4 w-4" />
			Active
			{#if activeBadgeCount > 0}
				<span
					class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full bg-info/15 text-info text-xs font-medium tabular-nums"
				>
					{activeBadgeCount}
				</span>
			{/if}
			{#if isPolling && activeTab === 'active'}
				<span class="polling-dot" aria-hidden="true"></span>
			{/if}
		</button>
		<button
			role="tab"
			class="tab-btn"
			class:tab-btn-active={activeTab === 'history'}
			aria-selected={activeTab === 'history'}
			onclick={() => switchTab('history')}
		>
			<History class="h-4 w-4" />
			History
			{#if historyTotal > 0}
				<span
					class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full bg-base-content/8 text-base-content/50 text-xs font-medium tabular-nums"
				>
					{historyTotal}
				</span>
			{/if}
		</button>
		<button
			role="tab"
			class="tab-btn"
			class:tab-btn-active={activeTab === 'wanted'}
			aria-selected={activeTab === 'wanted'}
			onclick={() => switchTab('wanted')}
		>
			<Radar class="h-4 w-4" />
			Wanted
			{#if wantedActiveCount > 0}
				<span
					class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full bg-base-content/8 text-base-content/50 text-xs font-medium tabular-nums"
				>
					{wantedActiveCount}
				</span>
			{/if}
		</button>
		{#if authStore.isAdmin}
			<button
				role="tab"
				class="tab-btn"
				class:tab-btn-active={activeTab === 'approvals'}
				aria-selected={activeTab === 'approvals'}
				onclick={() => switchTab('approvals')}
			>
				<ShieldCheck class="h-4 w-4" />
				Approvals
				{#if approvalCount > 0}
					<span
						class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full bg-warning/15 text-warning text-xs font-medium tabular-nums"
					>
						{approvalCount}
					</span>
				{/if}
			</button>
		{/if}
		{#if authStore.isAdmin}
			<button
				role="tab"
				class="tab-btn"
				class:tab-btn-active={activeTab === 'auto-download'}
				aria-selected={activeTab === 'auto-download'}
				onclick={() => switchTab('auto-download')}
			>
				<Heart class="h-4 w-4" />
				Auto-downloads
				{#if autoApprovalCount > 0}
					<span
						class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full bg-warning/15 text-warning text-xs font-medium tabular-nums"
					>
						{autoApprovalCount}
					</span>
				{/if}
			</button>
		{/if}
		{#if authStore.isTrusted}
			<button
				role="tab"
				class="tab-btn"
				class:tab-btn-active={activeTab === 'upgrades'}
				aria-selected={activeTab === 'upgrades'}
				onclick={() => switchTab('upgrades')}
			>
				<TrendingUp class="h-4 w-4" />
				Upgrades
				{#if upgradeItems.length > 0}
					<span
						class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full bg-base-content/8 text-base-content/50 text-xs font-medium tabular-nums"
					>
						{upgradeItems.length}
					</span>
				{/if}
			</button>
		{/if}
	</div>

	{#if activeTab === 'active'}
		<div in:fade={{ duration: 150 }} aria-live="polite">
			{#if activeError}
				<div class="alert alert-warning mb-4">
					<TriangleAlert class="h-5 w-5" />
					<span>{activeError}</span>
					<button class="btn btn-sm" onclick={() => void activeListQuery.refetch()}>Retry</button>
				</div>
			{/if}

			{#if activeLoading && activeItems.length === 0}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`active-loading-${i}`)}
						<div
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
							style="animation-delay: {i * 100}ms"
						>
							<div class="w-14 h-14 sm:w-18 sm:h-18 bg-base-300 rounded-lg"></div>
							<div class="flex-1">
								<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
								<div class="h-3 bg-base-300 rounded w-28 mb-1"></div>
								<div class="h-2.5 bg-base-300 rounded w-20"></div>
							</div>
							<div class="flex flex-col items-end gap-2">
								<div class="h-5 bg-base-300 rounded-full w-24"></div>
								<div class="h-1.5 bg-base-300 rounded w-36"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if activeItems.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div class="w-16 h-16 rounded-full bg-success/5 flex items-center justify-center mb-4">
						<CircleCheck class="h-8 w-8 text-success/30" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">All clear</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						No active downloads. Search for albums and request them to see progress here.
					</p>
				</div>
			{:else}
				{#if selectedList.length > 0}
					<div class="flex items-center gap-2 mb-3">
						<span class="text-xs text-base-content/60">
							{selectedList.length} selected
						</span>
						<button
							class="btn btn-sm btn-error btn-outline"
							disabled={batchCancelMutation.isPending}
							onclick={() => void handleBatchCancel()}
						>
							Cancel selected
						</button>
						<button
							class="btn btn-sm btn-ghost"
							onclick={() => (selectedKeys = new Set())}
						>
							Clear
						</button>
					</div>
				{/if}
				<div class="flex flex-col gap-2.5">
					{#each activeItems as item, i (itemRequestKey(item))}
						<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
							<RequestCard
								{item}
								mode="active"
								selectable
								selected={selectedKeys.has(itemRequestKey(item))}
								onselect={handleSelect}
								oncancel={authStore.isAdmin || item.user_id === authStore.user?.id
									? handleCancel
									: undefined}
							/>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{:else if activeTab === 'history'}
		<div in:fade={{ duration: 150 }}>
			<div class="flex flex-wrap items-center gap-2 mb-4">
				<select
					class="select select-bordered select-sm text-xs"
					aria-label="Filter by status"
					onchange={handleFilterChange}
				>
					<option value="">All statuses</option>
					<option value="imported">Imported</option>
					<option value="incomplete">Incomplete</option>
					<option value="failed">Failed</option>
					<option value="cancelled">Cancelled</option>
					<option value="rejected">Rejected</option>
				</select>

				<select
					class="select select-bordered select-sm text-xs"
					aria-label="Sort order"
					onchange={handleSortChange}
				>
					<option value="">Newest first</option>
					<option value="oldest">Oldest first</option>
					<option value="status">By status</option>
				</select>

				<div class="flex-1"></div>

				{#if historyTotalPages > 1}
					<Pagination
						current={historyPage}
						total={historyTotalPages}
						onchange={handleHistoryPageChange}
					/>
				{/if}
			</div>

			{#if historyError}
				<div class="alert alert-error mb-4">
					<span>{historyError}</span>
					<button class="btn btn-sm" onclick={() => void historyQuery.refetch()}>Retry</button>
				</div>
			{/if}

			{#if historyLoading && historyItems.length === 0}
				<div class="flex flex-col gap-2.5">
					{#each Array(5) as _, i (`history-loading-${i}`)}
						<div
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
							style="animation-delay: {i * 80}ms"
						>
							<div class="w-14 h-14 sm:w-18 sm:h-18 bg-base-300 rounded-lg"></div>
							<div class="flex-1">
								<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
								<div class="h-3 bg-base-300 rounded w-28"></div>
							</div>
							<div class="flex flex-col items-end gap-2">
								<div class="h-5 bg-base-300 rounded-full w-20"></div>
								<div class="h-3 bg-base-300 rounded w-28"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if historyItems.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div
						class="w-16 h-16 rounded-full bg-base-content/3 flex items-center justify-center mb-4"
					>
						<Clock class="h-8 w-8 text-base-content/15" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">No history yet</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						Completed and failed requests will appear here.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each historyItems as item (itemRequestKey(item))}
						<RequestCard
							{item}
							mode="history"
							watchState={itemKind(item) === 'album' &&
							['failed', 'incomplete', 'cancelled'].includes(item.status)
								? wantedStates.get(item.musicbrainz_id.toLowerCase())
								: undefined}
							onretry={authStore.isAdmin || item.user_id === authStore.user?.id
								? handleRetry
								: undefined}
							onclear={handleClear}
							onremoved={handleRemoved}
						/>
					{/each}
				</div>

				{#if historyTotalPages > 1}
					<div class="flex justify-center mt-6">
						<Pagination
							current={historyPage}
							total={historyTotalPages}
							onchange={handleHistoryPageChange}
						/>
					</div>
				{/if}
			{/if}
		</div>
	{:else if activeTab === 'wanted'}
		<div in:fade={{ duration: 150 }}>
			{#if wantedQuery.isError}
				<div class="alert alert-warning mb-4">
					<TriangleAlert class="h-5 w-5" />
					<span>Could not load the watchlist.</span>
					<button class="btn btn-sm" onclick={() => void wantedQuery.refetch()}>Retry</button>
				</div>
			{:else if wantedQuery.isPending}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`wanted-loading-${i}`)}
						<div
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
							style="animation-delay: {i * 100}ms"
						>
							<div class="w-14 h-14 sm:w-16 sm:h-16 bg-base-300 rounded-lg"></div>
							<div class="flex-1">
								<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
								<div class="h-3 bg-base-300 rounded w-28 mb-1"></div>
								<div class="h-2.5 bg-base-300 rounded w-52"></div>
							</div>
							<div class="flex gap-2">
								<div class="h-8 bg-base-300 rounded-btn w-24"></div>
								<div class="h-8 bg-base-300 rounded-btn w-16"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if wantedItems.length === 0 && wantedRetrying.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div
						class="w-16 h-16 rounded-full bg-base-content/3 flex items-center justify-center mb-4"
					>
						<Radar class="h-8 w-8 text-base-content/15" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">
						Nothing on the watchlist
					</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						When a request can't be found anywhere, DroppedNeedle keeps checking for it and lists it
						here.
					</p>
				</div>
			{:else}
				<p class="text-xs text-base-content/40 mb-3">
					Albums that couldn't be found are re-checked on a schedule. A copy that passes
					verification downloads by itself; near misses show up as candidates for you to review.
				</p>
				<div class="flex flex-col gap-2.5">
					{#each wantedRetrying as item, i (`retrying-${item.musicbrainz_id}`)}
						<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
							<WantedRetryingCard {item} />
						</div>
					{/each}
					{#each wantedItems as item, i (item.musicbrainz_id)}
						<div in:fly={{ y: 12, duration: 200, delay: (wantedRetrying.length + i) * 30 }}>
							<WantedWatchCard
								{item}
								busy={wantedBusy}
								onstop={authStore.isAdmin || item.user_id === authStore.user?.id
									? handleWantedStop
									: undefined}
								onresume={authStore.isAdmin || item.user_id === authStore.user?.id
									? handleWantedResume
									: undefined}
								onseen={handleWantedSeen}
							/>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{:else if activeTab === 'approvals' && authStore.isAdmin}
		<div in:fade={{ duration: 150 }}>
			{#if approvalError}
				<div class="alert alert-warning mb-4">
					<TriangleAlert class="h-5 w-5" />
					<span>{approvalError}</span>
					<button class="btn btn-sm" onclick={() => void approvalsQuery.refetch()}>Retry</button>
				</div>
			{/if}

			{#if approvalLoading && approvalItems.length === 0}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`approval-loading-${i}`)}
						<div
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
							style="animation-delay: {i * 100}ms"
						>
							<div class="w-14 h-14 sm:w-18 sm:h-18 bg-base-300 rounded-lg"></div>
							<div class="flex-1">
								<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
								<div class="h-3 bg-base-300 rounded w-28"></div>
							</div>
							<div class="flex gap-2">
								<div class="h-8 bg-base-300 rounded-btn w-20"></div>
								<div class="h-8 bg-base-300 rounded-btn w-20"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if approvalItems.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div class="w-16 h-16 rounded-full bg-success/5 flex items-center justify-center mb-4">
						<CircleCheck class="h-8 w-8 text-success/30" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">No pending approvals</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						Requests from regular users will appear here for your review.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each approvalItems as item, i (itemRequestKey(item))}
						<div
							in:fly={{ y: 12, duration: 200, delay: i * 30 }}
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box"
						>
							<div
								class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300"
							>
								<AlbumImage
									mbid={itemKind(item) === 'track' ? '' : item.musicbrainz_id}
									customUrl={null}
									alt={item.album_title}
									size="sm"
									rounded="lg"
									className="w-full h-full"
								/>
							</div>
							<div class="flex-1 min-w-0">
								{#if itemKind(item) === 'track'}
									<div class="flex items-center gap-1.5 min-w-0">
										<span class="badge badge-ghost badge-xs shrink-0">Track</span>
										<p class="font-semibold text-sm truncate">
											{item.track_title ?? 'Unknown track'}
										</p>
									</div>
									<p class="text-base-content/60 text-xs truncate">
										Album: {item.album_title}
									</p>
								{:else}
									<p class="font-semibold text-sm truncate">{item.album_title}</p>
								{/if}
								<p class="text-base-content/60 text-xs truncate">{item.artist_name}</p>
								<div class="flex items-center gap-1.5 flex-wrap">
									{#if item.year}
										<p class="text-base-content/40 text-xs">{item.year}</p>
									{/if}
									{#if item.requested_by_name}
										{#if item.year}<span class="text-base-content/20 text-xs">•</span>{/if}
										<p class="text-base-content/40 text-xs">
											Requested by {item.requested_by_name}
										</p>
									{/if}
								</div>
							</div>
							<div class="flex gap-2 shrink-0">
								<button
									class="btn btn-success btn-sm gap-1"
									onclick={() => void handleApprove(item.musicbrainz_id, itemKind(item))}
								>
									<Check class="h-3.5 w-3.5" />
									Approve
								</button>
								<button
									class="btn btn-error btn-sm btn-outline gap-1"
									onclick={() => void handleReject(item.musicbrainz_id, itemKind(item))}
								>
									<X class="h-3.5 w-3.5" />
									Reject
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{:else if activeTab === 'upgrades' && authStore.isTrusted}
		<div in:fade={{ duration: 150 }}>
			{#if cutoffUnmetQuery.isError}
				<div class="alert alert-warning mb-4">
					<TriangleAlert class="h-5 w-5" />
					<span>Could not load the upgrade worklist.</span>
					<button class="btn btn-sm" onclick={() => void cutoffUnmetQuery.refetch()}>Retry</button>
				</div>
			{:else if cutoffUnmetQuery.isPending}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`upgrade-loading-${i}`)}
						<div
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
							style="animation-delay: {i * 100}ms"
						>
							<div class="w-14 h-14 sm:w-16 sm:h-16 bg-base-300 rounded-lg"></div>
							<div class="flex-1">
								<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
								<div class="h-3 bg-base-300 rounded w-28"></div>
							</div>
							<div class="h-8 bg-base-300 rounded-btn w-32"></div>
						</div>
					{/each}
				</div>
			{:else if !cutoffUnmetQuery.data?.upgrade_allowed}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div
						class="w-16 h-16 rounded-full bg-base-content/3 flex items-center justify-center mb-4"
					>
						<TrendingUp class="h-8 w-8 text-base-content/15" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">Upgrades are off</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						Turn on "Allow automatic upgrades" in Settings → Download Clients to list albums below
						your quality cutoff.
					</p>
					{#if authStore.isAdmin}
						<a
							href={withBasePath('/settings?tab=download-client')}
							class="btn btn-sm btn-primary mt-4"
						>
							Open download settings
						</a>
					{/if}
				</div>
			{:else if upgradeItems.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div class="w-16 h-16 rounded-full bg-success/5 flex items-center justify-center mb-4">
						<CircleCheck class="h-8 w-8 text-success/30" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">
						Everything meets your cutoff
					</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						No album is below {tierLabel(cutoffUnmetQuery.data.cutoff)}. Albums that fall short will
						appear here.
					</p>
				</div>
			{:else}
				<p class="text-xs text-base-content/40 mb-3">
					Albums whose worst track is below your cutoff ({tierLabel(cutoffUnmetQuery.data.cutoff)}).
					"Find a better copy" only replaces a file when the new one is better quality; replaced
					files go to the recycle bin.
				</p>
				<div class="flex flex-col gap-2.5">
					{#each upgradeItems as item, i (item.release_group_mbid)}
						{@const queued = upgradeQueued.has(item.release_group_mbid)}
						<div
							in:fly={{ y: 12, duration: 200, delay: i * 30 }}
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box"
						>
							<div
								class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300"
							>
								<AlbumImage
									mbid={item.release_group_mbid}
									customUrl={null}
									alt={item.album_title ?? 'Album'}
									size="sm"
									rounded="lg"
									className="w-full h-full"
								/>
							</div>
							<div class="flex-1 min-w-0">
								<a
									href={withBasePath(`/album/${item.release_group_mbid}`)}
									class="block font-semibold text-sm truncate hover:text-accent hover:underline"
								>
									{item.album_title ?? 'Unknown album'}
								</a>
								<p class="text-base-content/60 text-xs truncate">
									{item.artist_name ?? 'Unknown artist'}{item.year ? ` • ${item.year}` : ''}
								</p>
								<p class="text-base-content/40 text-xs mt-0.5">
									<span class="text-warning/80">{tierLabel(item.current_tier)}</span>
									<span class="text-base-content/25">
										→ {tierLabel(cutoffUnmetQuery.data.cutoff)}</span
									>
									<span class="text-base-content/25"> • {item.track_count} tracks</span>
								</p>
							</div>
							<div class="shrink-0">
								<button
									class="btn btn-sm gap-1.5 {queued ? 'btn-ghost' : 'btn-primary btn-outline'}"
									disabled={queued || upgradeAlbum.isPending}
									onclick={() => void handleUpgrade(item)}
								>
									{#if queued}
										<Check class="h-3.5 w-3.5" />
										Queued
									{:else}
										<TrendingUp class="h-3.5 w-3.5" />
										Find a better copy
									{/if}
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{:else if activeTab === 'auto-download' && authStore.isAdmin}
		<div in:fade={{ duration: 150 }}>
			{#if autoApprovalsQuery.isError || mixApprovalsQuery.isError || batchApprovalsQuery.isError}
				<div class="alert alert-warning mb-4">
					<TriangleAlert class="h-5 w-5" />
					<span>Could not load auto-download approvals.</span>
					<button
						class="btn btn-sm"
						onclick={() => {
							void autoApprovalsQuery.refetch();
							void batchApprovalsQuery.refetch();
							void mixApprovalsQuery.refetch();
						}}>Retry</button
					>
				</div>
			{:else if autoApprovalsQuery.isPending || mixApprovalsQuery.isPending || batchApprovalsQuery.isPending}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`auto-approval-loading-${i}`)}
						<div
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box animate-pulse"
							style="animation-delay: {i * 100}ms"
						>
							<div class="w-14 h-14 sm:w-16 sm:h-16 bg-base-300 rounded-lg"></div>
							<div class="flex-1">
								<div class="h-4 bg-base-300 rounded w-44 mb-2"></div>
								<div class="h-3 bg-base-300 rounded w-28"></div>
							</div>
							<div class="flex gap-2">
								<div class="h-8 bg-base-300 rounded-btn w-20"></div>
								<div class="h-8 bg-base-300 rounded-btn w-20"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if autoApprovals.length === 0 && batchApprovals.length === 0 && mixApprovals.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div class="w-16 h-16 rounded-full bg-success/5 flex items-center justify-center mb-4">
						<Heart class="h-8 w-8 text-success/30" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">No pending approvals</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						When a user turns on artist auto-download or Weekly Mix auto-requests, it appears here
						for your review.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each autoApprovals as item (item.user_id + item.artist_mbid)}
						<div
							in:fly={{ y: 12, duration: 200 }}
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box"
						>
							<div
								class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300"
							>
								<ArtistImage
									mbid={item.artist_mbid}
									alt={item.artist_name}
									className="w-full h-full object-cover"
								/>
							</div>
							<div class="flex-1 min-w-0">
								<a
									href={withBasePath(`/artist/${item.artist_mbid}`)}
									class="block font-semibold text-sm truncate hover:text-accent hover:underline"
									title={item.artist_name}>{item.artist_name}</a
								>
								<div class="flex items-center gap-1.5 flex-wrap text-xs text-base-content/40">
									<span>{item.user_name ?? 'A user'}</span>
									<span class="text-base-content/20">•</span>
									<span>requested {approvalTimeAgo(item.requested_at)}</span>
								</div>
							</div>
							<div class="flex gap-2 shrink-0">
								<button
									class="btn btn-success btn-sm gap-1"
									disabled={approveAuto.isPending || rejectAuto.isPending}
									onclick={() =>
										approveAuto.mutate({
											userId: item.user_id,
											mbid: item.artist_mbid,
											artistName: item.artist_name
										})}
								>
									<Check class="h-3.5 w-3.5" />
									Approve
								</button>
								<button
									class="btn btn-error btn-sm btn-outline gap-1"
									disabled={approveAuto.isPending || rejectAuto.isPending}
									onclick={() =>
										rejectAuto.mutate({
											userId: item.user_id,
											mbid: item.artist_mbid,
											artistName: item.artist_name
										})}
								>
									<X class="h-3.5 w-3.5" />
									Reject
								</button>
							</div>
						</div>
					{/each}
					{#each batchApprovals as batch (batch.batch_id)}
						<div
							in:fly={{ y: 12, duration: 200 }}
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box"
						>
							<div
								class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg bg-base-300 flex items-center justify-center"
							>
								<CloudDownload class="h-6 w-6 text-accent/60" />
							</div>
							<div class="flex-1 min-w-0">
								<span class="block font-semibold text-sm">
									{batch.user_name ?? 'A user'} wants auto-download on {batch.artist_count} imported
									{batch.artist_count === 1 ? 'artist' : 'artists'}
								</span>
								<div class="flex items-center gap-1.5 flex-wrap text-xs text-base-content/40">
									<span>from Lidarr import</span>
									<span class="text-base-content/20">•</span>
									<span>requested {approvalTimeAgo(batch.requested_at)}</span>
								</div>
								{#if batch.sample_names.length > 0}
									<p class="mt-0.5 text-xs text-base-content/50 truncate">
										{batch.sample_names.join(', ')}{batch.artist_count > batch.sample_names.length
											? `, +${batch.artist_count - batch.sample_names.length} more`
											: ''}
									</p>
								{/if}
							</div>
							<div class="flex gap-2 shrink-0">
								<button
									class="btn btn-success btn-sm gap-1"
									disabled={approveBatch.isPending || rejectBatch.isPending}
									onclick={() =>
										approveBatch.mutate({
											batchId: batch.batch_id,
											userName: batch.user_name ?? 'A user',
											artistCount: batch.artist_count
										})}
								>
									<Check class="h-3.5 w-3.5" />
									Approve
								</button>
								<button
									class="btn btn-error btn-sm btn-outline gap-1"
									disabled={approveBatch.isPending || rejectBatch.isPending}
									onclick={() =>
										rejectBatch.mutate({
											batchId: batch.batch_id,
											userName: batch.user_name ?? 'A user',
											artistCount: batch.artist_count
										})}
								>
									<X class="h-3.5 w-3.5" />
									Reject
								</button>
							</div>
						</div>
					{/each}
					{#each mixApprovals as item (item.user_id)}
						<div
							in:fly={{ y: 12, duration: 200 }}
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box"
						>
							<div
								class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg bg-base-300 flex items-center justify-center"
							>
								<Sparkles class="h-6 w-6 text-accent/60" />
							</div>
							<div class="flex-1 min-w-0">
								<span class="block font-semibold text-sm truncate">Weekly Mix auto-requests</span>
								<div class="flex items-center gap-1.5 flex-wrap text-xs text-base-content/40">
									<span>{item.user_name ?? 'A user'}</span>
									<span class="text-base-content/20">•</span>
									<span>requested {approvalTimeAgo(item.requested_at)}</span>
								</div>
							</div>
							<div class="flex gap-2 shrink-0">
								<button
									class="btn btn-success btn-sm gap-1"
									disabled={approveMix.isPending || rejectMix.isPending}
									onclick={() =>
										approveMix.mutate({ userId: item.user_id, userName: item.user_name })}
								>
									<Check class="h-3.5 w-3.5" />
									Approve
								</button>
								<button
									class="btn btn-error btn-sm btn-outline gap-1"
									disabled={approveMix.isPending || rejectMix.isPending}
									onclick={() =>
										rejectMix.mutate({ userId: item.user_id, userName: item.user_name })}
								>
									<X class="h-3.5 w-3.5" />
									Reject
								</button>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{/if}
</div>

<Toast bind:show={toastShow} message={toastMessage} type={toastType} />

<style>
	.tab-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.5rem 0.85rem;
		font-size: 0.875rem;
		font-weight: 500;
		color: oklch(from var(--color-base-content) l c h / 0.4);
		border-bottom: 2px solid transparent;
		transition: all 0.15s ease;
		cursor: pointer;
		background: none;
		border-top: none;
		border-left: none;
		border-right: none;
		margin-bottom: -1px;
	}
	.tab-btn:hover {
		color: oklch(from var(--color-base-content) l c h / 0.7);
	}
	.tab-btn-active {
		color: oklch(from var(--color-primary) l c h / 1);
		border-bottom-color: oklch(from var(--color-primary) l c h / 1);
	}

	.polling-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: oklch(from var(--color-info) l c h / 0.7);
		animation: pulse-dot 1.5s ease-in-out infinite;
	}

	@keyframes pulse-dot {
		0%,
		100% {
			opacity: 0.3;
			transform: scale(0.8);
		}
		50% {
			opacity: 1;
			transform: scale(1.2);
		}
	}
</style>
