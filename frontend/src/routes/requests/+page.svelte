<script lang="ts">
	import { goto } from '$app/navigation';
	import ListPageHeader from '$lib/ui/ListPageHeader.svelte';
	import { page } from '$app/state';
	import { Download, Heart, History, Radar, Search, ShieldCheck, TrendingUp } from 'lucide-svelte';
	import {
		getActiveRequestCountQuery,
		getActiveRequestsQuery
	} from '$lib/features/requests/queries/RequestQueries.svelte';
	import { liveRequestStatus } from '$lib/features/requests/queries/liveStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import ActiveTab from '$lib/features/requests/components/ActiveTab.svelte';
	import ApprovalsTab from '$lib/features/requests/components/ApprovalsTab.svelte';
	import AutoDownloadTab from '$lib/features/requests/components/AutoDownloadTab.svelte';
	import HistoryTab from '$lib/features/requests/components/HistoryTab.svelte';
	import UpgradesTab from '$lib/features/requests/components/UpgradesTab.svelte';
	import WantedTab from '$lib/features/requests/components/WantedTab.svelte';
	import type { RequestsTab } from '$lib/features/requests/components/requestItems';

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
	// A click switches at once and rewrites the URL to match, through `goto`:
	// `replaceState` leaves `page.url` behind, and the sidebar highlights from it.
	let activeTab = $derived(tabFromUrl(page.url.searchParams.get('tab')));

	function switchTab(tab: RequestsTab) {
		activeTab = tab;
		const url = new URL(page.url);
		url.searchParams.set('tab', tab);
		void goto(url, { replaceState: true, noScroll: true, keepFocus: true });
	}

	// The header's counts show on every tab, so the page reads the list itself.
	const activeListQuery = getActiveRequestsQuery(() => !!authStore.user?.id);
	const activeSummary = $derived.by(() => {
		const items = activeListQuery.data?.items ?? [];
		return {
			count: activeListQuery.data?.count ?? items.length,
			downloading: items.filter((i) => liveRequestStatus(i) === 'downloading').length,
			searching: items.filter((i) => {
				if (i.waiting_reason) return false;
				const status = liveRequestStatus(i);
				return status === 'pending' || status === 'queued';
			}).length,
			fetching: activeListQuery.isFetching
		};
	});
	let counts = $state({ history: 0, wanted: 0, approvals: 0, auto: 0, upgrades: 0 });

	// The badge reads the cheap count endpoint and falls back to the list while it loads.
	const activeCountQuery = getActiveRequestCountQuery(() => !!authStore.user?.id);
	const activeBadgeCount = $derived(activeCountQuery.data?.count ?? activeSummary.count);

	interface TabEntry {
		id: RequestsTab;
		label: string;
		icon: typeof Download;
		count: number;
		/** `warning` counts read amber while their tab is closed. */
		tone: 'muted' | 'warning';
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
					tone: 'muted',
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

<svelte:head>
	<title>Requests - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<ListPageHeader
		title="Requests"
		lede="What you've asked for and where each one stands"
		ticket="DN / CC 3.2"
		stroke
		actions={activeSummary.downloading + activeSummary.searching > 0 ? inFlight : undefined}
	/>

	<div class="px-4 pb-12 sm:px-6 lg:px-8">
		<div class="rq-tabs" role="tablist" aria-label="Requests">
			{#each tabs as tab (tab.id)}
				{@const open = activeTab === tab.id}
				<button
					type="button"
					role="tab"
					class="rq-tab"
					aria-selected={open}
					onclick={() => switchTab(tab.id)}
				>
					<tab.icon class="size-5 shrink-0" aria-hidden="true" />
					{tab.label}
					{#if tab.count > 0}
						<span
							class="rq-tab__count"
							class:rq-tab__count--open={open}
							class:rq-tab__count--warning={!open && tab.tone === 'warning'}
						>
							{tab.count}
						</span>
					{/if}
					{#if tab.id === 'active' && open && activeSummary.fetching}
						<span class="rq-tab__dot" aria-hidden="true"></span>
					{/if}
				</button>
			{/each}
		</div>

		<ActiveTab active={activeTab === 'active'} />
		<HistoryTab active={activeTab === 'history'} oncount={(n) => (counts.history = n)} />
		<WantedTab active={activeTab === 'wanted'} oncount={(n) => (counts.wanted = n)} />
		{#if authStore.isAdmin}
			<ApprovalsTab
				active={activeTab === 'approvals'}
				oncount={(n) => (counts.approvals = n)}
				autoCount={counts.auto}
				onopenauto={() => switchTab('auto-download')}
			/>
			<AutoDownloadTab active={activeTab === 'auto-download'} oncount={(n) => (counts.auto = n)} />
		{/if}
		{#if authStore.isTrusted}
			<UpgradesTab active={activeTab === 'upgrades'} oncount={(n) => (counts.upgrades = n)} />
		{/if}
	</div>
</div>

{#snippet inFlight()}
	<span class="rq-flight">
		<Download class="size-5" aria-hidden="true" />
		{activeSummary.downloading} downloading
	</span>
	<span class="rq-flight" class:rq-flight--warning={activeSummary.searching > 0}>
		<Search class="size-5" aria-hidden="true" />
		{activeSummary.searching} searching
	</span>
{/snippet}

<style>
	/* the in-flight counts: plain status in the header buttons' shape */
	.rq-flight {
		display: inline-flex;
		align-items: center;
		gap: 0.625rem;
		height: 2.875rem;
		padding-inline: 1.125rem;
		border: 2px solid var(--color-base-content);
		border-radius: 0.625rem;
		font-family: var(--font-display);
		font-size: 1.1875rem;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--color-base-content);
		font-synthesis: none;
	}
	.rq-flight--warning {
		border-color: var(--color-warning);
		color: var(--color-warning);
	}

	/* display caps tabs with an acid underline; the row scrolls sideways */
	.rq-tabs {
		display: flex;
		gap: 0.5rem;
		margin-bottom: 1.75rem;
		overflow-x: auto;
		border-bottom: 1px solid var(--dn-line);
		scrollbar-width: none;
		overscroll-behavior-x: contain;
	}
	.rq-tabs::-webkit-scrollbar {
		display: none;
	}
	.rq-tab {
		position: relative;
		display: inline-flex;
		flex: none;
		align-items: center;
		gap: 0.625rem;
		padding: 0.875rem 1.25rem 0.9375rem;
		font-family: var(--font-display);
		font-size: 1.3125rem;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		white-space: nowrap;
		color: var(--dn-muted);
		cursor: pointer;
		font-synthesis: none;
		transition: color 150ms;
	}
	.rq-tab:first-child {
		padding-left: 0.5rem;
	}
	.rq-tab:hover {
		color: var(--color-base-content);
	}
	.rq-tab:focus-visible {
		outline-offset: -2px;
	}
	.rq-tab[aria-selected='true'] {
		color: var(--dn-title);
	}
	.rq-tab[aria-selected='true']::after {
		content: '';
		position: absolute;
		inset: auto 0 -1px;
		height: 3px;
		background-color: var(--dn-title);
	}

	.rq-tab__count {
		display: inline-grid;
		place-items: center;
		min-width: 1.625rem;
		height: 1.625rem;
		padding-inline: 0.375rem;
		border-radius: 0.375rem;
		background-color: var(--color-neutral);
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 700;
		letter-spacing: 0;
		font-variant-numeric: tabular-nums;
		color: var(--color-base-content);
	}
	.rq-tab__count--open {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
	.rq-tab__count--warning {
		background-color: color-mix(in oklab, var(--color-warning) 18%, transparent);
		color: var(--color-warning);
	}

	.rq-tab__dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background-color: var(--dn-title);
		animation: rq-pulse 1.5s ease-in-out infinite;
	}
	@keyframes rq-pulse {
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

	@media (max-width: 767px) {
		.rq-flight {
			height: 2.25rem;
			justify-content: center;
			gap: 0.375rem;
			padding-inline: 0.5rem;
			font-size: 0.9375rem;
		}
		.rq-flight :global(svg) {
			width: 1rem;
			height: 1rem;
		}
		.rq-tabs {
			gap: 0;
			margin-inline: -1rem;
			padding-inline: 0.5rem;
			margin-bottom: 1.25rem;
		}
		.rq-tab {
			gap: 0.375rem;
			padding: 0.75rem 0.75rem 0.8125rem;
			font-size: 1.0625rem;
		}
		.rq-tab:first-child {
			padding-left: 0.5rem;
		}
		.rq-tab :global(svg) {
			width: 1rem;
			height: 1rem;
		}
		.rq-tab__count {
			min-width: 1.375rem;
			height: 1.375rem;
			font-size: 0.75rem;
		}
	}
</style>
