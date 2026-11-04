<script lang="ts">
	import { ChevronLeft, RefreshCw } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import LiveUpdatingBadge from './LiveUpdatingBadge.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Breadcrumb {
		label: string;
		href?: string;
	}

	interface Props {
		title: Snippet;
		subtitle: string;
		loading?: boolean;
		refreshing?: boolean;
		isUpdating?: boolean;
		lastUpdated?: Date | null;
		refreshLabel?: string;
		breadcrumbs?: Breadcrumb[];
		backHref?: string;
		backLabel?: string;
		// when provided, replaces the default refresh button entirely
		actions?: Snippet;
		onRefresh?: () => void;
	}

	let {
		title,
		subtitle,
		loading = false,
		refreshing = false,
		isUpdating = false,
		lastUpdated = null,
		refreshLabel = 'Refresh',
		breadcrumbs = [],
		backHref,
		backLabel = 'Back',
		actions,
		onRefresh
	}: Props = $props();
</script>

<div class="relative mb-6">
	<div class="relative px-4 py-8 sm:px-6 sm:py-12 lg:px-8">
		{#if backHref}
			<IconButton
				href={withBasePath(backHref)}
				icon={ChevronLeft}
				label={backLabel}
				class="-ml-2 mb-3"
			/>
		{/if}
		<div class="flex items-start justify-between">
			<div>
				{#if breadcrumbs.length}
					<nav class="mb-2 flex items-center gap-1 font-mono text-xs uppercase text-muted">
						{#each breadcrumbs as crumb, i (crumb.label)}
							{#if crumb.href}
								<a href={crumb.href} class="link link-hover">{crumb.label}</a>
							{:else}
								<span>{crumb.label}</span>
							{/if}
							{#if i < breadcrumbs.length - 1}<span class="opacity-50">/</span>{/if}
						{/each}
					</nav>
				{/if}
				<h1
					class="mb-3 font-display text-4xl leading-[0.92] text-title uppercase sm:text-5xl lg:text-6xl"
				>
					{@render title()}
				</h1>
				<p class="max-w-xl text-sm text-muted sm:text-base">
					{subtitle}
				</p>
			</div>
			<div class="flex flex-wrap items-center gap-2">
				{#if actions}
					{@render actions()}
				{:else}
					{#if isUpdating}
						<LiveUpdatingBadge label="Refreshing" />
					{:else if lastUpdated && !loading}
						<span class="hidden font-mono text-xs text-muted sm:inline">
							Updated {formatLastUpdated(lastUpdated)}
						</span>
					{/if}
					{#if onRefresh}
						<Button
							variant="neutral"
							size="sm"
							class="gap-1"
							onclick={onRefresh}
							disabled={refreshing || loading}
							title={refreshLabel}
							aria-label={refreshLabel}
						>
							<RefreshCw class="h-4 w-4 {refreshing ? 'motion-safe:animate-spin' : ''}" />
							<span class="hidden sm:inline">{refreshLabel}</span>
						</Button>
					{/if}
				{/if}
			</div>
		</div>
	</div>
</div>
