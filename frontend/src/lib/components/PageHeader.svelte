<script lang="ts">
	/*
	 * PageHeader — legacy-API masthead, re-rendered in the new design language
	 * (quiet wash, display Title Case, mono status line). Same props as always,
	 * so every existing consumer picks the new look up without changes.
	 * `gradientClass` is accepted but ignored — surface identity now comes from
	 * the shared token wash.
	 */
	import { RefreshCw } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import LiveUpdatingBadge from './LiveUpdatingBadge.svelte';

	interface Breadcrumb {
		label: string;
		href?: string;
	}

	interface Props {
		title: Snippet;
		subtitle: string;
		gradientClass?: string;
		loading?: boolean;
		refreshing?: boolean;
		isUpdating?: boolean;
		lastUpdated?: Date | null;
		refreshLabel?: string;
		breadcrumbs?: Breadcrumb[];
		// when provided, replaces the default refresh button entirely
		actions?: Snippet;
		onRefresh?: () => void;
	}

	let {
		title,
		subtitle,
		// legacy API surface; surface identity now comes from the shared token wash
		gradientClass: _gradientClass = '',
		loading = false,
		refreshing = false,
		isUpdating = false,
		lastUpdated = null,
		refreshLabel = 'Refresh',
		breadcrumbs = [],
		actions,
		onRefresh
	}: Props = $props();
</script>

<header class="dn-pagehead">
	<div class="dn-pagehead__wash" aria-hidden="true"></div>
	<div
		class="relative flex flex-wrap items-start justify-between gap-4 px-4 py-8 sm:px-6 sm:py-10 lg:px-8"
	>
		<div class="min-w-0">
			{#if breadcrumbs.length}
				<nav
					class="mb-2 flex items-center gap-1.5 font-mono text-[0.62rem] font-bold uppercase tracking-[0.18em] text-base-content/45"
					aria-label="Breadcrumb"
				>
					{#each breadcrumbs as crumb, i (crumb.label)}
						{#if crumb.href}
							<a href={crumb.href} class="hover:text-base-content">{crumb.label}</a>
						{:else}
							<span>{crumb.label}</span>
						{/if}
						{#if i < breadcrumbs.length - 1}<span class="opacity-50">/</span>{/if}
					{/each}
				</nav>
			{/if}
			<h1 class="hero-title font-display text-3xl font-bold tracking-tight sm:text-4xl">
				{@render title()}
			</h1>
			<p class="mt-2 max-w-xl text-sm text-base-content/60 sm:text-base">
				{subtitle}
			</p>
		</div>
		<div class="flex items-center gap-2">
			{#if actions}
				{@render actions()}
			{:else}
				{#if isUpdating}
					<LiveUpdatingBadge label="Refreshing" />
				{:else if lastUpdated && !loading}
					<span class="hidden text-xs text-base-content/45 sm:inline">
						Updated {formatLastUpdated(lastUpdated)}
					</span>
				{/if}
				{#if onRefresh}
					<button
						class="btn btn-sm btn-primary gap-1"
						onclick={onRefresh}
						disabled={refreshing || loading}
						title={refreshLabel}
					>
						<RefreshCw class="h-4 w-4 {refreshing ? 'animate-spin' : ''}" />
						<span class="hidden sm:inline">{refreshLabel}</span>
					</button>
				{/if}
			{/if}
		</div>
	</div>
</header>

<style>
	.dn-pagehead {
		position: relative;
		isolation: isolate;
		overflow: hidden;
		margin-bottom: 1.5rem;
	}
	.dn-pagehead__wash {
		position: absolute;
		inset: 0;
		z-index: -1;
		background:
			radial-gradient(
				circle at 15% -40%,
				oklch(from var(--color-primary) l c h / 0.08),
				transparent 55%
			),
			radial-gradient(
				circle at 85% -60%,
				oklch(from var(--color-accent) l c h / 0.06),
				transparent 55%
			);
	}
</style>
