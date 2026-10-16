<script lang="ts">
	import { replaceState } from '$app/navigation';
	import { page } from '$app/state';
	import { Download, Heart, History, Radar, Search, ShieldCheck, TrendingUp } from 'lucide-svelte';
	import { getActiveRequestCountQuery } from '$lib/queries/requests/RequestQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import ActiveTab, { type ActiveSummary } from './ActiveTab.svelte';
	import ApprovalsTab from './ApprovalsTab.svelte';
	import AutoDownloadTab from './AutoDownloadTab.svelte';
	import HistoryTab from './HistoryTab.svelte';
	import UpgradesTab from './UpgradesTab.svelte';
	import WantedTab from './WantedTab.svelte';
	import type { RequestsTab } from './requestItems';

	function tabFromUrl(param: string | null): RequestsTab {
		switch (param) {
			case 'history':
			case 'wanted':
				return param;
			case 'approvals':
			case 'auto-download':
				return authStore.isAdmin ? param : 'active';
			case 'upgrades':
				return authStore.isTrusted ? param : 'active';
			default:
				return 'active';
		}
	}

	// The URL names the open tab, so a sidebar link or a reload lands on it.
	// A click switches at once and rewrites the URL to match.
	let activeTab = $derived(tabFromUrl(page.url.searchParams.get('tab')));

	function switchTab(tab: RequestsTab) {
		activeTab = tab;
		const url = new URL(page.url);
		url.searchParams.set('tab', tab);
		replaceState(url, {});
	}

	let activeSummary = $state<ActiveSummary>({
		count: 0,
		downloading: 0,
		searching: 0,
		fetching: false
	});
	let counts = $state({ history: 0, wanted: 0, approvals: 0, auto: 0, upgrades: 0 });

	// The badge reads the cheap count endpoint and falls back to the list while it loads.
	const activeCountQuery = getActiveRequestCountQuery(() => !!authStore.user?.id);
	const activeBadgeCount = $derived(activeCountQuery.data?.count ?? activeSummary.count);

	const badgeTones = {
		info: 'bg-info/15 text-info',
		warning: 'bg-warning/15 text-warning',
		muted: 'bg-base-content/8 text-base-content/50'
	};

	interface TabEntry {
		id: RequestsTab;
		label: string;
		icon: typeof Download;
		count: number;
		tone: keyof typeof badgeTones;
		show: boolean;
	}

	const tabs = $derived(
		(
			[
				{
					id: 'active',
					label: 'Active',
					icon: Download,
					count: activeBadgeCount,
					tone: 'info',
					show: true
				},
				{
					id: 'history',
					label: 'History',
					icon: History,
					count: counts.history,
					tone: 'muted',
					show: true
				},
				{
					id: 'wanted',
					label: 'Wanted',
					icon: Radar,
					count: counts.wanted,
					tone: 'muted',
					show: true
				},
				{
					id: 'approvals',
					label: 'Approvals',
					icon: ShieldCheck,
					count: counts.approvals,
					tone: 'warning',
					show: authStore.isAdmin
				},
				{
					id: 'auto-download',
					label: 'Auto-downloads',
					icon: Heart,
					count: counts.auto,
					tone: 'warning',
					show: authStore.isAdmin
				},
				{
					id: 'upgrades',
					label: 'Upgrades',
					icon: TrendingUp,
					count: counts.upgrades,
					tone: 'muted',
					show: authStore.isTrusted
				}
			] satisfies TabEntry[]
		).filter((tab) => tab.show)
	);
</script>

<div class="container mx-auto px-4 sm:px-6 lg:px-8 py-6 sm:py-8">
	<div class="flex flex-col sm:flex-row justify-between items-start sm:items-center gap-3 mb-6">
		<div>
			<h1 class="text-2xl sm:text-3xl font-bold text-base-content">Requests</h1>
			<p class="text-base-content/50 text-sm mt-0.5">
				What you've asked for and where each one stands
			</p>
		</div>
		{#if activeSummary.count > 0}
			<div class="flex items-center gap-3 text-xs text-base-content/50">
				{#if activeSummary.downloading > 0}
					<span class="flex items-center gap-1.5">
						<Download class="h-3.5 w-3.5 text-info" />
						{activeSummary.downloading} downloading
					</span>
				{/if}
				{#if activeSummary.searching > 0}
					<span class="flex items-center gap-1.5">
						<Search class="h-3.5 w-3.5 text-warning" />
						{activeSummary.searching} searching
					</span>
				{/if}
			</div>
		{/if}
	</div>

	<div
		class="flex items-center gap-1 mb-6 border-b border-base-content/5 pb-px overflow-x-auto"
		role="tablist"
	>
		{#each tabs as tab (tab.id)}
			<button
				role="tab"
				class="tab-btn"
				class:tab-btn-active={activeTab === tab.id}
				aria-selected={activeTab === tab.id}
				onclick={() => switchTab(tab.id)}
			>
				<tab.icon class="h-4 w-4" />
				{tab.label}
				{#if tab.count > 0}
					<span
						class="inline-flex items-center justify-center min-w-5 h-5 px-1.5 rounded-full {badgeTones[
							tab.tone
						]} text-xs font-medium tabular-nums"
					>
						{tab.count}
					</span>
				{/if}
				{#if tab.id === 'active' && activeTab === 'active' && activeSummary.fetching}
					<span class="polling-dot" aria-hidden="true"></span>
				{/if}
			</button>
		{/each}
	</div>

	<ActiveTab active={activeTab === 'active'} onsummary={(summary) => (activeSummary = summary)} />
	<HistoryTab active={activeTab === 'history'} oncount={(n) => (counts.history = n)} />
	<WantedTab active={activeTab === 'wanted'} oncount={(n) => (counts.wanted = n)} />
	{#if authStore.isAdmin}
		<ApprovalsTab active={activeTab === 'approvals'} oncount={(n) => (counts.approvals = n)} />
		<AutoDownloadTab active={activeTab === 'auto-download'} oncount={(n) => (counts.auto = n)} />
	{/if}
	{#if authStore.isTrusted}
		<UpgradesTab active={activeTab === 'upgrades'} oncount={(n) => (counts.upgrades = n)} />
	{/if}
</div>

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
