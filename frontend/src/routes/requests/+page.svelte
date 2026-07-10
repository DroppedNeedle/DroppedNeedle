<script lang="ts">
	import { onMount, onDestroy, untrack } from 'svelte';
	import { SvelteMap } from 'svelte/reactivity';
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
		Radar,
		Search,
		ShieldCheck,
		Check,
		X,
		Heart,
		Sparkles,
		DownloadCloud,
		TrendingUp,
		Inbox
	} from 'lucide-svelte';
	import PageHero from '$lib/ui/PageHero.svelte';
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
	import { isAbortError } from '$lib/utils/errorHandling';
	import { libraryStore } from '$lib/stores/library';
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
		wantedItems.filter((i) => i.state === 'watching' || i.state === 'dormant').length +
			wantedRetrying.length
	);
	// mbid (lowercased) -> chip state for History rows: a terminal-looking request
	// that's actually still being worked on must never read as dead
	const wantedStates = $derived.by(() => {
		const map = new SvelteMap<string, 'retrying' | 'watching'>();
		for (const entry of wantedRetrying) {
			map.set(entry.release_group_mbid.toLowerCase(), 'retrying');
		}
		for (const watch of wantedItems) {
			if (watch.state === 'watching' || watch.state === 'dormant') {
				map.set(watch.release_group_mbid.toLowerCase(), 'watching');
			}
		}
		return map;
	});
	const stopWatch = createStopWatchMutation();
	const resumeWatch = createResumeWatchMutation();
	const markWantedSeen = createMarkWantedSeenMutation();
	const wantedBusy = $derived(stopWatch.isPending || resumeWatch.isPending);

	function handleWantedStop(item: WantedWatchItem) {
		stopWatch.mutate({ mbid: item.release_group_mbid, albumTitle: item.album_title });
	}

	function handleWantedResume(item: WantedWatchItem) {
		resumeWatch.mutate({ mbid: item.release_group_mbid, albumTitle: item.album_title });
	}

	function handleWantedSeen(item: WantedWatchItem) {
		markWantedSeen.mutate({ mbid: item.release_group_mbid, albumTitle: item.album_title });
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
		} else if (tabParam === 'wanted') {
			switchTab('wanted');
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

	onDestroy(() => {
		stopPolling();
		abortActiveLoad();
		abortHistoryLoad();
		abortApprovalsLoad();
		document.removeEventListener('visibilitychange', handleVisibility);
	});
</script>

<PageHero
	title="Requests"
	subtitle="What you've asked for and where each one stands."
	eyebrow="On order"
	tint="var(--color-info)"
>
	{#snippet icon()}
		<Inbox class="h-7 w-7" />
	{/snippet}
	{#snippet actions()}
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
	{/snippet}
</PageHero>

<div class="px-4 pb-12 sm:px-6 lg:px-8">
	<div class="mb-6 flex items-center gap-2 overflow-x-auto pb-1" role="tablist">
		<button
			role="tab"
			class="btn btn-sm shrink-0 gap-1.5 rounded-full {activeTab === 'active'
				? 'btn-primary'
				: 'btn-ghost bg-base-content/6'}"
			aria-selected={activeTab === 'active'}
			onclick={() => switchTab('active')}
		>
			<Download class="h-4 w-4" />
			Active
			{#if activeCount > 0}
				<span
					class="inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-xs font-medium tabular-nums {activeTab ===
					'active'
						? 'bg-primary-content/12 text-primary-content'
						: 'bg-info/15 text-info'}"
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
			class="btn btn-sm shrink-0 gap-1.5 rounded-full {activeTab === 'history'
				? 'btn-primary'
				: 'btn-ghost bg-base-content/6'}"
			aria-selected={activeTab === 'history'}
			onclick={() => switchTab('history')}
		>
			<History class="h-4 w-4" />
			History
			{#if historyTotal > 0}
				<span
					class="inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-xs font-medium tabular-nums {activeTab ===
					'history'
						? 'bg-primary-content/12 text-primary-content'
						: 'bg-base-content/8 text-base-content/50'}"
				>
					{historyTotal}
				</span>
			{/if}
		</button>
		<button
			role="tab"
			class="btn btn-sm shrink-0 gap-1.5 rounded-full {activeTab === 'wanted'
				? 'btn-primary'
				: 'btn-ghost bg-base-content/6'}"
			aria-selected={activeTab === 'wanted'}
			onclick={() => switchTab('wanted')}
		>
			<Radar class="h-4 w-4" />
			Wanted
			{#if wantedActiveCount > 0}
				<span
					class="inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-xs font-medium tabular-nums {activeTab ===
					'wanted'
						? 'bg-primary-content/12 text-primary-content'
						: 'bg-base-content/8 text-base-content/50'}"
				>
					{wantedActiveCount}
				</span>
			{/if}
		</button>
		{#if authStore.isAdmin}
			<button
				role="tab"
				class="btn btn-sm shrink-0 gap-1.5 rounded-full {activeTab === 'approvals'
					? 'btn-primary'
					: 'btn-ghost bg-base-content/6'}"
				aria-selected={activeTab === 'approvals'}
				onclick={() => switchTab('approvals')}
			>
				<ShieldCheck class="h-4 w-4" />
				Approvals
				{#if approvalCount > 0}
					<span
						class="inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-xs font-medium tabular-nums {activeTab ===
						'approvals'
							? 'bg-primary-content/12 text-primary-content'
							: 'bg-warning/15 text-warning'}"
					>
						{approvalCount}
					</span>
				{/if}
			</button>
		{/if}
		{#if authStore.isAdmin}
			<button
				role="tab"
				class="btn btn-sm shrink-0 gap-1.5 rounded-full {activeTab === 'auto-download'
					? 'btn-primary'
					: 'btn-ghost bg-base-content/6'}"
				aria-selected={activeTab === 'auto-download'}
				onclick={() => switchTab('auto-download')}
			>
				<Heart class="h-4 w-4" />
				Auto-downloads
				{#if autoApprovalCount > 0}
					<span
						class="inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-xs font-medium tabular-nums {activeTab ===
						'auto-download'
							? 'bg-primary-content/12 text-primary-content'
							: 'bg-warning/15 text-warning'}"
					>
						{autoApprovalCount}
					</span>
				{/if}
			</button>
		{/if}
		{#if authStore.isTrusted}
			<button
				role="tab"
				class="btn btn-sm shrink-0 gap-1.5 rounded-full {activeTab === 'upgrades'
					? 'btn-primary'
					: 'btn-ghost bg-base-content/6'}"
				aria-selected={activeTab === 'upgrades'}
				onclick={() => switchTab('upgrades')}
			>
				<TrendingUp class="h-4 w-4" />
				Upgrades
				{#if upgradeItems.length > 0}
					<span
						class="inline-flex h-5 min-w-5 items-center justify-center rounded-full px-1.5 text-xs font-medium tabular-nums {activeTab ===
						'upgrades'
							? 'bg-primary-content/12 text-primary-content'
							: 'bg-base-content/8 text-base-content/50'}"
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
				<div class="alert alert-warning mb-4 rounded-2xl">
					<TriangleAlert class="h-5 w-5" />
					<span>{activeError}</span>
					<button class="btn btn-sm rounded-full" onclick={loadActive}>Retry</button>
				</div>
			{/if}

			{#if activeLoading && activeItems.length === 0}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`active-loading-${i}`)}
						<div
							class="flex animate-pulse items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 sm:gap-4 sm:p-4"
							style="animation-delay: {i * 100}ms"
						>
							<div class="h-14 w-14 rounded-lg bg-base-content/8 sm:h-18 sm:w-18"></div>
							<div class="flex-1">
								<div class="mb-2 h-4 w-44 rounded bg-base-content/8"></div>
								<div class="mb-1 h-3 w-28 rounded bg-base-content/8"></div>
								<div class="h-2.5 w-20 rounded bg-base-content/8"></div>
							</div>
							<div class="flex flex-col items-end gap-2">
								<div class="h-5 w-24 rounded-full bg-base-content/8"></div>
								<div class="h-1.5 w-36 rounded bg-base-content/8"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if activeItems.length === 0}
				<div
					class="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 px-6 py-16 text-center"
				>
					<div class="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-accent/8">
						<CheckCircle class="h-7 w-7 text-accent/70" />
					</div>
					<h2 class="font-display text-lg font-semibold text-base-content/70">All clear</h2>
					<p class="mt-1.5 max-w-xs text-sm text-base-content/45">
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
			<div class="mb-4 flex flex-wrap items-center gap-2">
				<select
					class="select select-sm w-auto rounded-full border-base-content/10 bg-base-200/60 text-xs"
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
					class="select select-sm w-auto rounded-full border-base-content/10 bg-base-200/60 text-xs"
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
				<div class="alert alert-error mb-4 rounded-2xl">
					<span>{historyError}</span>
					<button class="btn btn-sm rounded-full" onclick={loadHistory}>Retry</button>
				</div>
			{/if}

			{#if historyLoading && historyItems.length === 0}
				<div class="flex flex-col gap-2.5">
					{#each Array(5) as _, i (`history-loading-${i}`)}
						<div
							class="flex animate-pulse items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 sm:gap-4 sm:p-4"
							style="animation-delay: {i * 80}ms"
						>
							<div class="h-14 w-14 rounded-lg bg-base-content/8 sm:h-18 sm:w-18"></div>
							<div class="flex-1">
								<div class="mb-2 h-4 w-44 rounded bg-base-content/8"></div>
								<div class="h-3 w-28 rounded bg-base-content/8"></div>
							</div>
							<div class="flex flex-col items-end gap-2">
								<div class="h-5 w-20 rounded-full bg-base-content/8"></div>
								<div class="h-3 w-28 rounded bg-base-content/8"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if historyItems.length === 0}
				<div
					class="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 px-6 py-16 text-center"
				>
					<div
						class="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-base-content/6"
					>
						<Clock class="h-7 w-7 text-base-content/25" />
					</div>
					<h2 class="font-display text-lg font-semibold text-base-content/70">No history yet</h2>
					<p class="mt-1.5 max-w-xs text-sm text-base-content/45">
						Completed and failed requests will appear here.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each historyItems as item (item.musicbrainz_id)}
						<RequestCard
							{item}
							mode="history"
							watchState={['failed', 'incomplete', 'cancelled'].includes(item.status)
								? wantedStates.get(item.musicbrainz_id.toLowerCase())
								: undefined}
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
					<div class="mt-6 flex justify-center">
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
				<div class="alert alert-warning mb-4 rounded-2xl">
					<TriangleAlert class="h-5 w-5" />
					<span>Could not load the watchlist.</span>
					<button class="btn btn-sm rounded-full" onclick={() => void wantedQuery.refetch()}
						>Retry</button
					>
				</div>
			{:else if wantedQuery.isPending}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`wanted-loading-${i}`)}
						<div
							class="flex animate-pulse items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 sm:gap-4 sm:p-4"
							style="animation-delay: {i * 100}ms"
						>
							<div class="h-14 w-14 rounded-lg bg-base-content/8 sm:h-16 sm:w-16"></div>
							<div class="flex-1">
								<div class="mb-2 h-4 w-44 rounded bg-base-content/8"></div>
								<div class="mb-1 h-3 w-28 rounded bg-base-content/8"></div>
								<div class="h-2.5 w-52 rounded bg-base-content/8"></div>
							</div>
							<div class="flex gap-2">
								<div class="h-8 w-24 rounded-full bg-base-content/8"></div>
								<div class="h-8 w-16 rounded-full bg-base-content/8"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if wantedItems.length === 0 && wantedRetrying.length === 0}
				<div
					class="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 px-6 py-16 text-center"
				>
					<div
						class="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-base-content/6"
					>
						<Radar class="h-7 w-7 text-base-content/25" />
					</div>
					<h2 class="font-display text-lg font-semibold text-base-content/70">
						Nothing on the watchlist
					</h2>
					<p class="mt-1.5 max-w-xs text-sm text-base-content/45">
						When a request can't be found anywhere, DroppedNeedle keeps checking for it and lists it
						here.
					</p>
				</div>
			{:else}
				<p class="mb-3 text-xs text-base-content/45">
					Albums that couldn't be found are re-checked on a schedule. A copy that passes
					verification downloads by itself; near misses show up as candidates for you to review.
				</p>
				<div class="flex flex-col gap-2.5">
					{#each wantedRetrying as item, i (`retrying-${item.release_group_mbid}`)}
						<div in:fly={{ y: 12, duration: 200, delay: i * 30 }}>
							<WantedRetryingCard
								{item}
								ownerName={authStore.isAdmin && item.user_id !== authStore.user?.id
									? (item.user_name ?? undefined)
									: undefined}
							/>
						</div>
					{/each}
					{#each wantedItems as item, i (item.release_group_mbid)}
						<div in:fly={{ y: 12, duration: 200, delay: (wantedRetrying.length + i) * 30 }}>
							<WantedWatchCard
								{item}
								busy={wantedBusy}
								ownerName={authStore.isAdmin && item.user_id !== authStore.user?.id
									? (item.user_name ?? undefined)
									: undefined}
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
				<div class="alert alert-warning mb-4 rounded-2xl">
					<TriangleAlert class="h-5 w-5" />
					<span>{approvalError}</span>
					<button class="btn btn-sm rounded-full" onclick={loadApprovals}>Retry</button>
				</div>
			{/if}

			{#if approvalLoading && approvalItems.length === 0}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`approval-loading-${i}`)}
						<div
							class="flex animate-pulse items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 sm:gap-4 sm:p-4"
							style="animation-delay: {i * 100}ms"
						>
							<div class="h-14 w-14 rounded-lg bg-base-content/8 sm:h-18 sm:w-18"></div>
							<div class="flex-1">
								<div class="mb-2 h-4 w-44 rounded bg-base-content/8"></div>
								<div class="h-3 w-28 rounded bg-base-content/8"></div>
							</div>
							<div class="flex gap-2">
								<div class="h-8 w-20 rounded-full bg-base-content/8"></div>
								<div class="h-8 w-20 rounded-full bg-base-content/8"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if approvalItems.length === 0}
				<div
					class="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 px-6 py-16 text-center"
				>
					<div class="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-accent/8">
						<CheckCircle class="h-7 w-7 text-accent/70" />
					</div>
					<h2 class="font-display text-lg font-semibold text-base-content/70">
						No pending approvals
					</h2>
					<p class="mt-1.5 max-w-xs text-sm text-base-content/45">
						Requests from regular users will appear here for your review.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each approvalItems as item, i (item.musicbrainz_id)}
						<div
							in:fly={{ y: 12, duration: 200, delay: i * 30 }}
							class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30 sm:gap-4 sm:p-4"
						>
							<div
								class="h-14 w-14 shrink-0 overflow-hidden rounded-lg bg-base-300 ring-1 ring-base-content/10 sm:h-16 sm:w-16"
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
							<div class="min-w-0 flex-1">
								<p class="truncate text-sm font-semibold">{item.album_title}</p>
								<p class="truncate text-xs text-base-content/60">{item.artist_name}</p>
								<div class="flex flex-wrap items-center gap-1.5">
									{#if item.year}
										<p class="text-xs text-base-content/40">{item.year}</p>
									{/if}
									{#if item.requested_by_name}
										{#if item.year}<span class="text-xs text-base-content/20">•</span>{/if}
										<p class="text-xs text-base-content/40">
											Requested by {item.requested_by_name}
										</p>
									{/if}
								</div>
							</div>
							<div class="flex shrink-0 gap-2">
								<button
									class="btn btn-success btn-sm gap-1 rounded-full"
									onclick={() => void handleApprove(item.musicbrainz_id)}
								>
									<Check class="h-3.5 w-3.5" />
									Approve
								</button>
								<button
									class="btn btn-error btn-sm btn-outline gap-1 rounded-full"
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
				<div class="alert alert-warning mb-4 rounded-2xl">
					<TriangleAlert class="h-5 w-5" />
					<span>Could not load the upgrade worklist.</span>
					<button class="btn btn-sm rounded-full" onclick={() => void cutoffUnmetQuery.refetch()}
						>Retry</button
					>
				</div>
			{:else if cutoffUnmetQuery.isPending}
				<div class="flex flex-col gap-2.5">
					{#each Array(3) as _, i (`upgrade-loading-${i}`)}
						<div
							class="flex animate-pulse items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 sm:gap-4 sm:p-4"
							style="animation-delay: {i * 100}ms"
						>
							<div class="h-14 w-14 rounded-lg bg-base-content/8 sm:h-16 sm:w-16"></div>
							<div class="flex-1">
								<div class="mb-2 h-4 w-44 rounded bg-base-content/8"></div>
								<div class="h-3 w-28 rounded bg-base-content/8"></div>
							</div>
							<div class="h-8 w-32 rounded-full bg-base-content/8"></div>
						</div>
					{/each}
				</div>
			{:else if !cutoffUnmetQuery.data?.upgrade_allowed}
				<div
					class="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 px-6 py-16 text-center"
				>
					<div
						class="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-base-content/6"
					>
						<TrendingUp class="h-7 w-7 text-base-content/25" />
					</div>
					<h2 class="font-display text-lg font-semibold text-base-content/70">Upgrades are off</h2>
					<p class="mt-1.5 max-w-xs text-sm text-base-content/45">
						Turn on "Allow automatic upgrades" in Settings → Download Clients to list albums below
						your quality cutoff.
					</p>
					{#if authStore.isAdmin}
						<a
							href="/settings?tab=download-client"
							class="btn btn-sm btn-primary mt-4 rounded-full"
						>
							Open download settings
						</a>
					{/if}
				</div>
			{:else if upgradeItems.length === 0}
				<div
					class="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 px-6 py-16 text-center"
				>
					<div class="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-accent/8">
						<CheckCircle class="h-7 w-7 text-accent/70" />
					</div>
					<h2 class="font-display text-lg font-semibold text-base-content/70">
						Everything meets your cutoff
					</h2>
					<p class="mt-1.5 max-w-xs text-sm text-base-content/45">
						No album is below {tierLabel(cutoffUnmetQuery.data.cutoff)}. Albums that fall short will
						appear here.
					</p>
				</div>
			{:else}
				<p class="mb-3 text-xs text-base-content/45">
					Albums whose worst track is below your cutoff ({tierLabel(cutoffUnmetQuery.data.cutoff)}).
					"Find a better copy" only replaces a file when the new one is better quality; replaced
					files go to the recycle bin.
				</p>
				<div class="flex flex-col gap-2.5">
					{#each upgradeItems as item, i (item.release_group_mbid)}
						{@const queued = upgradeQueued.has(item.release_group_mbid)}
						<div
							in:fly={{ y: 12, duration: 200, delay: i * 30 }}
							class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30 sm:gap-4 sm:p-4"
						>
							<div
								class="h-14 w-14 shrink-0 overflow-hidden rounded-lg bg-base-300 ring-1 ring-base-content/10 sm:h-16 sm:w-16"
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
							<div class="min-w-0 flex-1">
								<a
									href="/album/{item.release_group_mbid}"
									class="block truncate text-sm font-semibold transition-colors hover:text-primary"
								>
									{item.album_title ?? 'Unknown album'}
								</a>
								<p class="truncate text-xs text-base-content/60">
									{item.artist_name ?? 'Unknown artist'}{item.year ? ` • ${item.year}` : ''}
								</p>
								<p class="mt-0.5 font-mono text-[0.65rem] uppercase tracking-wider">
									<span class="text-warning/80">{tierLabel(item.current_tier)}</span>
									<span class="text-base-content/25">
										→ {tierLabel(cutoffUnmetQuery.data.cutoff)}</span
									>
									<span class="text-base-content/25"> • {item.track_count} tracks</span>
								</p>
							</div>
							<div class="shrink-0">
								<button
									class="btn btn-sm gap-1.5 rounded-full {queued
										? 'btn-ghost'
										: 'btn-primary btn-outline'}"
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
				<div class="alert alert-warning mb-4 rounded-2xl">
					<TriangleAlert class="h-5 w-5" />
					<span>Could not load auto-download approvals.</span>
					<button
						class="btn btn-sm rounded-full"
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
							class="flex animate-pulse items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 sm:gap-4 sm:p-4"
							style="animation-delay: {i * 100}ms"
						>
							<div class="h-14 w-14 rounded-lg bg-base-content/8 sm:h-16 sm:w-16"></div>
							<div class="flex-1">
								<div class="mb-2 h-4 w-44 rounded bg-base-content/8"></div>
								<div class="h-3 w-28 rounded bg-base-content/8"></div>
							</div>
							<div class="flex gap-2">
								<div class="h-8 w-20 rounded-full bg-base-content/8"></div>
								<div class="h-8 w-20 rounded-full bg-base-content/8"></div>
							</div>
						</div>
					{/each}
				</div>
			{:else if autoApprovals.length === 0 && batchApprovals.length === 0 && mixApprovals.length === 0}
				<div
					class="flex min-h-60 flex-col items-center justify-center rounded-2xl border border-dashed border-base-content/12 px-6 py-16 text-center"
				>
					<div class="mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-accent/8">
						<Heart class="h-7 w-7 text-accent/70" />
					</div>
					<h2 class="font-display text-lg font-semibold text-base-content/70">
						No pending approvals
					</h2>
					<p class="mt-1.5 max-w-xs text-sm text-base-content/45">
						When a user turns on artist auto-download or Weekly Mix auto-requests, it appears here
						for your review.
					</p>
				</div>
			{:else}
				<div class="flex flex-col gap-2.5">
					{#each autoApprovals as item (item.user_id + item.artist_mbid)}
						<div
							in:fly={{ y: 12, duration: 200 }}
							class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30 sm:gap-4 sm:p-4"
						>
							<div
								class="h-14 w-14 shrink-0 overflow-hidden rounded-lg bg-base-300 ring-1 ring-base-content/10 sm:h-16 sm:w-16"
							>
								<ArtistImage
									mbid={item.artist_mbid}
									alt={item.artist_name}
									className="w-full h-full object-cover"
								/>
							</div>
							<div class="min-w-0 flex-1">
								<a
									href="/artist/{item.artist_mbid}"
									class="block truncate text-sm font-semibold transition-colors hover:text-primary"
									title={item.artist_name}>{item.artist_name}</a
								>
								<div class="flex flex-wrap items-center gap-1.5 text-xs text-base-content/40">
									<span>{item.user_name ?? 'A user'}</span>
									<span class="text-base-content/20">•</span>
									<span>requested {approvalTimeAgo(item.requested_at)}</span>
								</div>
							</div>
							<div class="flex shrink-0 gap-2">
								<button
									class="btn btn-success btn-sm gap-1 rounded-full"
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
									class="btn btn-error btn-sm btn-outline gap-1 rounded-full"
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
							class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30 sm:gap-4 sm:p-4"
						>
							<div
								class="flex h-14 w-14 shrink-0 items-center justify-center rounded-lg bg-base-content/6 sm:h-16 sm:w-16"
							>
								<DownloadCloud class="h-6 w-6 text-accent/60" />
							</div>
							<div class="min-w-0 flex-1">
								<span class="block text-sm font-semibold">
									{batch.user_name ?? 'A user'} wants auto-download on {batch.artist_count} imported
									{batch.artist_count === 1 ? 'artist' : 'artists'}
								</span>
								<div class="flex flex-wrap items-center gap-1.5 text-xs text-base-content/40">
									<span>from Lidarr import</span>
									<span class="text-base-content/20">•</span>
									<span>requested {approvalTimeAgo(batch.requested_at)}</span>
								</div>
								{#if batch.sample_names.length > 0}
									<p class="mt-0.5 truncate text-xs text-base-content/50">
										{batch.sample_names.join(', ')}{batch.artist_count > batch.sample_names.length
											? `, +${batch.artist_count - batch.sample_names.length} more`
											: ''}
									</p>
								{/if}
							</div>
							<div class="flex shrink-0 gap-2">
								<button
									class="btn btn-success btn-sm gap-1 rounded-full"
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
									class="btn btn-error btn-sm btn-outline gap-1 rounded-full"
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
							class="flex items-center gap-3 rounded-2xl border border-base-content/8 bg-base-200/50 p-3 transition-colors hover:border-primary/30 sm:gap-4 sm:p-4"
						>
							<div
								class="flex h-14 w-14 shrink-0 items-center justify-center rounded-lg bg-base-content/6 sm:h-16 sm:w-16"
							>
								<Sparkles class="h-6 w-6 text-accent/60" />
							</div>
							<div class="min-w-0 flex-1">
								<span class="block truncate text-sm font-semibold">Weekly Mix auto-requests</span>
								<div class="flex flex-wrap items-center gap-1.5 text-xs text-base-content/40">
									<span>{item.user_name ?? 'A user'}</span>
									<span class="text-base-content/20">•</span>
									<span>requested {approvalTimeAgo(item.requested_at)}</span>
								</div>
							</div>
							<div class="flex shrink-0 gap-2">
								<button
									class="btn btn-success btn-sm gap-1 rounded-full"
									disabled={approveMix.isPending || rejectMix.isPending}
									onclick={() =>
										approveMix.mutate({ userId: item.user_id, userName: item.user_name })}
								>
									<Check class="h-3.5 w-3.5" />
									Approve
								</button>
								<button
									class="btn btn-error btn-sm btn-outline gap-1 rounded-full"
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
	.polling-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: currentColor;
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

	@media (prefers-reduced-motion: reduce) {
		.polling-dot {
			animation: none;
		}
	}
</style>
