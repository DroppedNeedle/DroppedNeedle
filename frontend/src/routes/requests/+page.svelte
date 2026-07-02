<script lang="ts">
	import { onMount, onDestroy, untrack } from 'svelte';
	import { page } from '$app/state';
	import { fade, fly } from 'svelte/transition';
	import RequestCard from '$lib/components/RequestCard.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import Toast from '$lib/components/Toast.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import type { ActiveRequestItem, RequestHistoryItem } from '$lib/types';
	import {
		TriangleAlert,
		CheckCircle,
		Clock,
		Download,
		History,
		Search,
		ShieldCheck,
		Check,
		X,
		Heart,
		TrendingUp
	} from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import {
		fetchActiveRequests,
		fetchRequestHistory,
		cancelRequest,
		retryRequest,
		clearHistoryItem,
		fetchPendingApprovals,
		approveRequest,
		rejectRequest,
		notifyPendingApprovalCountChanged
	} from '$lib/utils/requestsApi';
	import { getAutoDownloadApprovalsQuery } from '$lib/queries/following/AdminApprovalsQueries.svelte';
	import {
		createApproveAutoDownloadMutation,
		createRejectAutoDownloadMutation
	} from '$lib/queries/following/AdminApprovalsMutations.svelte';
	import { isAbortError } from '$lib/utils/errorHandling';
	import { libraryStore } from '$lib/stores/library';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getCutoffUnmetQuery,
		requestUpgradeAlbum
	} from '$lib/queries/downloads/UpgradeQueries.svelte';
	import { QUALITY_TIERS } from '$lib/components/settings/qualityTiers';

	type RequestsTab = 'active' | 'history' | 'approvals' | 'auto-download' | 'upgrades';
	let activeTab = $state<RequestsTab>('active');

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
	const autoApprovalCount = $derived(autoApprovalsQuery.data?.count ?? 0);
	const approveAuto = createApproveAutoDownloadMutation();
	const rejectAuto = createRejectAutoDownloadMutation();

	function approvalTimeAgo(epochSeconds: number): string {
		const diff = Date.now() / 1000 - epochSeconds;
		if (diff < 60) return 'just now';
		if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
		if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
		return `${Math.floor(diff / 86400)}d ago`;
	}

	let activeItems = $state<ActiveRequestItem[]>([]);
	let activeCount = $state(0);
	let prevActiveCount = 0;
	let activeLoading = $state(true);
	let activeError = $state<string | null>(null);

	let historyItems = $state<RequestHistoryItem[]>([]);
	let historyTotal = $state(0);
	let historyPage = $state(1);
	const historyPageSize = 20;
	let historyTotalPages = $state(1);
	let historyLoading = $state(true);
	let historyError = $state<string | null>(null);
	let historyFilter = $state<string | undefined>(undefined);
	let historySort = $state<string | undefined>(undefined);

	let pollInterval: ReturnType<typeof setInterval> | null = null;
	let activeAbortController: AbortController | null = null;
	let historyAbortController: AbortController | null = null;
	let activeRequestId = 0;
	let historyRequestId = 0;
	let toastShow = $state(false);
	let toastMessage = $state('');
	let toastType = $state<'success' | 'error' | 'info'>('success');
	let isPolling = $state(false);

	let approvalItems = $state<ActiveRequestItem[]>([]);
	let approvalCount = $state(0);
	let approvalLoading = $state(true);
	let approvalError = $state<string | null>(null);
	let approvalAbortController: AbortController | null = null;

	const downloadingCount = $derived(activeItems.filter((i) => i.status === 'downloading').length);
	const pendingCount = $derived(
		activeItems.filter((i) => i.status === 'pending' || i.status === 'queued').length
	);

	function abortActiveLoad() {
		if (activeAbortController) {
			activeAbortController.abort();
			activeAbortController = null;
		}
	}

	function abortHistoryLoad() {
		if (historyAbortController) {
			historyAbortController.abort();
			historyAbortController = null;
		}
	}

	function showToast(message: string, type: 'success' | 'error' | 'info' = 'success') {
		toastMessage = message;
		toastType = type;
		toastShow = true;
	}

	async function loadActive() {
		const requestId = ++activeRequestId;
		abortActiveLoad();
		const controller = new AbortController();
		activeAbortController = controller;
		isPolling = true;
		try {
			const data = await fetchActiveRequests(controller.signal);
			if (controller.signal.aborted || requestId !== activeRequestId) {
				return;
			}
			activeItems = data.items;
			activeCount = data.count;
			if (prevActiveCount > 0 && data.count < prevActiveCount) {
				libraryStore.refresh();
			}
			prevActiveCount = data.count;
			activeError = null;
		} catch (e) {
			if (isAbortError(e)) {
				return;
			}
			activeError = "Couldn't load active requests";
		} finally {
			if (!controller.signal.aborted && requestId === activeRequestId) {
				activeLoading = false;
			}
			if (activeAbortController === controller) {
				activeAbortController = null;
			}
			setTimeout(() => {
				isPolling = false;
			}, 500);
		}
	}

	async function loadHistory() {
		const requestId = ++historyRequestId;
		abortHistoryLoad();
		const controller = new AbortController();
		historyAbortController = controller;
		historyLoading = true;
		try {
			const data = await fetchRequestHistory(
				historyPage,
				historyPageSize,
				historyFilter,
				controller.signal,
				historySort
			);
			if (controller.signal.aborted || requestId !== historyRequestId) {
				return;
			}
			historyItems = data.items;
			historyTotal = data.total;
			historyTotalPages = data.total_pages;
			historyError = null;
		} catch (e) {
			if (isAbortError(e)) {
				return;
			}
			historyError = "Couldn't load request history";
		} finally {
			if (!controller.signal.aborted && requestId === historyRequestId) {
				historyLoading = false;
			}
			if (historyAbortController === controller) {
				historyAbortController = null;
			}
		}
	}

	function startPolling() {
		stopPolling();
		void loadActive();
		pollInterval = setInterval(loadActive, 5000);
	}

	function stopPolling() {
		if (pollInterval) {
			clearInterval(pollInterval);
			pollInterval = null;
		}
		abortActiveLoad();
	}

	function handleVisibility() {
		if (document.hidden) {
			stopPolling();
		} else if (activeTab === 'active') {
			startPolling();
		}
	}

	function abortApprovalsLoad() {
		if (approvalAbortController) {
			approvalAbortController.abort();
			approvalAbortController = null;
		}
	}

	async function loadApprovals() {
		abortApprovalsLoad();
		const controller = new AbortController();
		approvalAbortController = controller;
		approvalLoading = true;
		try {
			const data = await fetchPendingApprovals(controller.signal);
			if (controller.signal.aborted) return;
			approvalItems = data.items;
			approvalCount = data.count;
			notifyPendingApprovalCountChanged();
			approvalError = null;
		} catch (e) {
			if (isAbortError(e)) return;
			approvalError = "Couldn't load pending approvals";
		} finally {
			if (!controller.signal.aborted) approvalLoading = false;
			if (approvalAbortController === controller) approvalAbortController = null;
		}
	}

	async function handleApprove(mbid: string) {
		try {
			const result = await approveRequest(mbid);
			if (result.success) {
				showToast(result.message);
				approvalItems = approvalItems.filter((i) => i.musicbrainz_id !== mbid);
				approvalCount = approvalItems.length;
				notifyPendingApprovalCountChanged();
			} else {
				showToast(result.message, 'error');
			}
		} catch {
			showToast('Could not approve that request', 'error');
		}
	}

	async function handleReject(mbid: string) {
		try {
			const result = await rejectRequest(mbid);
			if (result.success) {
				showToast(result.message, 'info');
				approvalItems = approvalItems.filter((i) => i.musicbrainz_id !== mbid);
				approvalCount = approvalItems.length;
				notifyPendingApprovalCountChanged();
			} else {
				showToast(result.message, 'error');
			}
		} catch {
			showToast('Could not reject that request', 'error');
		}
	}

	function switchTab(tab: RequestsTab) {
		activeTab = tab;
		if (tab === 'active') {
			abortHistoryLoad();
			abortApprovalsLoad();
			startPolling();
		} else if (tab === 'history') {
			stopPolling();
			abortApprovalsLoad();
			void loadHistory();
		} else if (tab === 'approvals') {
			stopPolling();
			abortHistoryLoad();
			void loadApprovals();
		} else {
			// auto-download: the TanStack query fetches itself once the tab is active
			stopPolling();
			abortHistoryLoad();
			abortApprovalsLoad();
		}
	}

	async function handleCancel(mbid: string) {
		try {
			const result = await cancelRequest(mbid);
			if (result.success) {
				showToast(result.message);
				activeItems = activeItems.filter((i) => i.musicbrainz_id !== mbid);
				activeCount = activeItems.length;
			} else {
				showToast(result.message, 'error');
			}
		} catch {
			showToast("Couldn't cancel that request", 'error');
		}
	}

	async function handleRetry(mbid: string) {
		try {
			const result = await retryRequest(mbid);
			if (result.success) {
				showToast(result.message);
				await Promise.all([loadHistory(), loadActive()]);
			} else {
				showToast(result.message, 'error');
			}
		} catch {
			showToast("Couldn't retry that request", 'error');
		}
	}

	async function handleClear(mbid: string) {
		try {
			const result = await clearHistoryItem(mbid);
			if (result.success) {
				showToast('Removed from history');
				historyItems = historyItems.filter((i) => i.musicbrainz_id !== mbid);
				historyTotal = Math.max(0, historyTotal - 1);
			} else {
				showToast("Couldn't remove that item", 'error');
			}
		} catch {
			showToast("Couldn't remove that item from history", 'error');
		}
	}

	function handleRemoved() {
		void loadHistory();
	}

	function handleHistoryPageChange(page: number) {
		historyPage = page;
		void loadHistory();
	}

	function handleFilterChange(e: Event) {
		const value = (e.target as HTMLSelectElement).value;
		historyFilter = value || undefined;
		historyPage = 1;
		void loadHistory();
	}

	function handleSortChange(e: Event) {
		const value = (e.target as HTMLSelectElement).value;
		historySort = value || undefined;
		historyPage = 1;
		void loadHistory();
	}

	onMount(() => {
		document.addEventListener('visibilitychange', handleVisibility);
		const tabParam = page.url.searchParams.get('tab');
		if (tabParam === 'approvals' && authStore.isAdmin) {
			switchTab('approvals');
		} else {
			startPolling();
			if (authStore.isAdmin) void loadApprovals();
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
		const target: 'active' | 'history' | 'approvals' =
			tabParam === 'approvals' && authStore.isAdmin
				? 'approvals'
				: tabParam === 'history'
					? 'history'
					: 'active';
		if (untrack(() => activeTab) !== target) {
			switchTab(target);
		}
	});

	onDestroy(() => {
		stopPolling();
		abortActiveLoad();
		abortHistoryLoad();
		abortApprovalsLoad();
		document.removeEventListener('visibilitychange', handleVisibility);
	});
</script>

<div class="container mx-auto px-4 sm:px-6 lg:px-8 py-6 sm:py-8">
	<div class="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-3 mb-6">
		<div>
			<h1 class="text-2xl sm:text-3xl font-bold text-base-content">Requests</h1>
			<p class="text-base-content/50 text-sm mt-0.5">Track your album requests and downloads</p>
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

	<div class="flex items-center gap-1 mb-6 border-b border-base-content/5 pb-px" role="tablist">
		<button
			role="tab"
			class="tab-btn"
			class:tab-btn-active={activeTab === 'active'}
			aria-selected={activeTab === 'active'}
			onclick={() => switchTab('active')}
		>
			<Download class="h-4 w-4" />
			Active
			{#if activeCount > 0}
				<span
					class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full bg-info/15 text-info text-xs font-medium tabular-nums"
				>
					{activeCount}
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
					<button class="btn btn-sm" onclick={loadActive}>Retry</button>
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
						<CheckCircle class="h-8 w-8 text-success/30" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">All clear</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						No active downloads. Search for albums and request them to see progress here.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each activeItems as item, i (item.musicbrainz_id)}
						<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
							<RequestCard
								{item}
								mode="active"
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
					<option value="importFailed">Import Failed</option>
					<option value="importBlocked">Import Blocked</option>
					<option value="cancelled">Cancelled</option>
					{#if authStore.isAdmin}
						<option value="reimportable">Can reimport</option>
					{/if}
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
					<button class="btn btn-sm" onclick={loadHistory}>Retry</button>
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
					{#each historyItems as item (item.musicbrainz_id)}
						<RequestCard
							{item}
							mode="history"
							onretry={authStore.isAdmin || item.user_id === authStore.user?.id
								? handleRetry
								: undefined}
							onclear={handleClear}
							onremoved={handleRemoved}
							onreimported={loadHistory}
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
	{:else if activeTab === 'approvals' && authStore.isAdmin}
		<div in:fade={{ duration: 150 }}>
			{#if approvalError}
				<div class="alert alert-warning mb-4">
					<TriangleAlert class="h-5 w-5" />
					<span>{approvalError}</span>
					<button class="btn btn-sm" onclick={loadApprovals}>Retry</button>
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
						<CheckCircle class="h-8 w-8 text-success/30" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">No pending approvals</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						Requests from regular users will appear here for your review.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each approvalItems as item, i (item.musicbrainz_id)}
						<div
							in:fly={{ y: 12, duration: 200, delay: i * 30 }}
							class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box"
						>
							<div
								class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300"
							>
								<AlbumImage
									mbid={item.musicbrainz_id}
									customUrl={item.cover_url ?? null}
									alt={item.album_title}
									size="sm"
									rounded="lg"
									className="w-full h-full"
								/>
							</div>
							<div class="flex-1 min-w-0">
								<p class="font-semibold text-sm truncate">{item.album_title}</p>
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
									onclick={() => void handleApprove(item.musicbrainz_id)}
								>
									<Check class="h-3.5 w-3.5" />
									Approve
								</button>
								<button
									class="btn btn-error btn-sm btn-outline gap-1"
									onclick={() => void handleReject(item.musicbrainz_id)}
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
						<a href="/settings?tab=download-client" class="btn btn-sm btn-primary mt-4">
							Open download settings
						</a>
					{/if}
				</div>
			{:else if upgradeItems.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div class="w-16 h-16 rounded-full bg-success/5 flex items-center justify-center mb-4">
						<CheckCircle class="h-8 w-8 text-success/30" />
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
									href="/album/{item.release_group_mbid}"
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
			{#if autoApprovalsQuery.isError}
				<div class="alert alert-warning mb-4">
					<TriangleAlert class="h-5 w-5" />
					<span>Could not load auto-download approvals.</span>
					<button class="btn btn-sm" onclick={() => void autoApprovalsQuery.refetch()}>Retry</button
					>
				</div>
			{:else if autoApprovalsQuery.isPending}
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
			{:else if autoApprovals.length === 0}
				<div class="flex flex-col items-center justify-center min-h-60 text-center py-16">
					<div class="w-16 h-16 rounded-full bg-success/5 flex items-center justify-center mb-4">
						<Heart class="h-8 w-8 text-success/30" />
					</div>
					<h2 class="text-lg font-semibold mb-1.5 text-base-content/50">
						No pending auto-download approvals
					</h2>
					<p class="text-base-content/30 text-sm max-w-xs">
						When a user turns on auto-download for an artist, it appears here for your review.
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
									href="/artist/{item.artist_mbid}"
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
