<script lang="ts">
	import { ChevronLeft, RefreshCw } from 'lucide-svelte';
	import type { Snippet } from 'svelte';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import LiveUpdatingBadge from './LiveUpdatingBadge.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	const NOISE =
		"url('data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 200 200%22><filter id=%22n%22><feTurbulence type=%22fractalNoise%22 baseFrequency=%220.9%22 numOctaves=%224%22 stitchTiles=%22stitch%22/></filter><rect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23n)%22 opacity=%220.5%22/></svg>')";

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
		backHref?: string;
		backLabel?: string;
		// when provided, replaces the default refresh button entirely
		actions?: Snippet;
		onRefresh?: () => void;
	}

	let {
		title,
		subtitle,
		gradientClass = 'bg-gradient-to-br from-primary/30 via-secondary/20 to-accent/10',
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

<div class="relative mb-6 overflow-hidden {gradientClass}">
	<div class="absolute inset-0 bg-linear-to-t from-base-100 to-transparent"></div>
	<div
		class="absolute inset-0 opacity-[0.03]"
		style:background-image={NOISE}
		style:background-size="200px"
	></div>
	<div
		class="absolute left-0 top-1/2 -translate-y-1/2 w-96 h-48 rounded-full bg-primary/10 blur-3xl pointer-events-none"
	></div>
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
					<nav class="mb-2 flex items-center gap-1 text-xs text-base-content/60">
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
				<h1 class="mb-2 text-3xl font-bold sm:text-4xl lg:text-5xl">
					{@render title()}
				</h1>
				<p class="max-w-xl text-sm text-base-content/70 sm:text-base">
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
						<span class="hidden text-xs text-base-content/50 sm:inline">
							Updated {formatLastUpdated(lastUpdated)}
						</span>
					{/if}
					{#if onRefresh}
						<Button
							variant="primary"
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
