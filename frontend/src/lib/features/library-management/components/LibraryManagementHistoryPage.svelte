<script lang="ts">
	import '$lib/features/library-management/components/library-management.css';
	import { ChevronRight, FolderCog, ShieldAlert } from 'lucide-svelte';

	import BackButton from '$lib/ui/BackButton.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		getLibraryManagementOperationsQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/features/library-management/queries/LibraryManagementQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { isRecord, titleManagementValue } from './LibraryManagementDisplay';

	let origin = $state('');
	let profileId = $state('');
	let rootId = $state('');
	let operationState = $state('');
	let mode = $state('');
	let createdFrom = $state('');
	let createdTo = $state('');

	const settingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const policyQuery = getTargetLibrarySettingsQuery(() => authStore.isAdmin);
	const historyQuery = getLibraryManagementOperationsQuery(
		() => authStore.user?.id,
		() => ({
			limit: 25,
			origin: origin || undefined,
			profileId: profileId || undefined,
			rootId: rootId || undefined,
			state: operationState || undefined,
			mode: mode || undefined,
			createdFrom: dateBoundary(createdFrom, false),
			createdTo: dateBoundary(createdTo, true)
		})
	);

	const historyItems = $derived(historyQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const roots = $derived(policyQuery.data?.library_roots ?? []);

	function dateBoundary(value: string, end: boolean): number | undefined {
		if (!value) return undefined;
		const suffix = end ? 'T23:59:59' : 'T00:00:00';
		const timestamp = new Date(`${value}${suffix}`).getTime();
		return Number.isFinite(timestamp) ? timestamp / 1000 : undefined;
	}

	function operationHref(jobId: string, value: string, terminalCode: string | null): string {
		return value === 'ready' || terminalCode === 'PREVIEW_DISCARDED'
			? withBasePath(`/library/management/previews/${encodeURIComponent(jobId)}`)
			: withBasePath(`/library/management/operations/${encodeURIComponent(jobId)}`);
	}

	function rootLabel(value: string | null): string {
		return (
			roots.find((root) => root.id === value)?.label ??
			(value ? 'Unavailable root' : 'Within source roots')
		);
	}

	function scopeLabel(selection: Record<string, unknown>, targetRootId: string | null): string {
		const kind = typeof selection.kind === 'string' ? titleManagementValue(selection.kind) : null;
		const ids = Array.isArray(selection.ids) ? selection.ids.length : 0;
		const source = kind ? `${kind}${ids ? ` · ${ids} selected` : ''}` : 'Pinned operation scope';
		return `${source} → ${rootLabel(targetRootId)}`;
	}

	function formatDate(value: number): string {
		return new Date(value * 1000).toLocaleString();
	}

	function latestTimestampLabel(state: string): string {
		if (['succeeded', 'failed', 'cancelled', 'stopped'].includes(state)) return 'Finished';
		if (state === 'ready') return 'Ready';
		return 'Updated';
	}

	function stateTone(state: string): 'success' | 'warning' | 'error' | 'neutral' {
		if (state === 'failed') return 'error';
		if (state === 'succeeded') return 'success';
		if (state === 'ready') return 'warning';
		return 'neutral';
	}
</script>

<svelte:head><title>Organization history · DroppedNeedle</title></svelte:head>

<div class="management-preview-shell px-4 py-8 sm:px-6 lg:px-8">
	<main class="mx-auto max-w-7xl space-y-6">
		<BackButton fallback="/library/management?tab=organize" />

		<header>
			<p class="management-kicker">
				<ShieldAlert class="h-3.5 w-3.5" /> Administrator audit trail
			</p>
			<h1 class="lm-page-title mt-2">Organization history</h1>
			<p class="mt-3 max-w-3xl text-muted">
				Previews, writes, undo, baseline restores, and collision resolutions. Scans are not included
				here.
			</p>
		</header>

		<section class="lm-bordered hp-filters" aria-labelledby="management-history-filters">
			<h2 id="management-history-filters" class="lm-cap-title">Filter write-system history</h2>
			<div class="mt-3 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
				<label class="hp-field"
					><span>Origin</span><select class="select select-sm" bind:value={origin}
						><option value="">All origins</option><option value="manual">Manual</option><option
							value="acquisition">Acquisition</option
						><option value="drop_import">Drop import</option><option value="scan_discovered"
							>Scan discovered</option
						></select
					></label
				>
				<label class="hp-field"
					><span>Profile</span><select class="select select-sm" bind:value={profileId}
						><option value="">All profiles</option
						>{#each settingsQuery.data?.profiles ?? [] as profile (profile.id)}<option
								value={profile.id}>{profile.name}</option
							>{/each}</select
					></label
				>
				<label class="hp-field"
					><span>Root scope</span><select class="select select-sm" bind:value={rootId}
						><option value="">All roots</option>{#each roots as root (root.id)}<option
								value={root.id}>{root.label}</option
							>{/each}</select
					></label
				>
				<label class="hp-field"
					><span>State</span><select class="select select-sm" bind:value={operationState}
						><option value="">All states</option
						>{#each ['queued', 'running', 'paused', 'ready', 'succeeded', 'failed', 'cancelled', 'stopped'] as option (option)}<option
								value={option}>{titleManagementValue(option)}</option
							>{/each}</select
					></label
				>
				<label class="hp-field"
					><span>Mode</span><select class="select select-sm" bind:value={mode}
						><option value="">All modes</option
						>{#each ['preview', 'apply', 'automatic_apply', 'undo', 'baseline_restore', 'duplicate_resolution'] as option (option)}<option
								value={option}>{titleManagementValue(option)}</option
							>{/each}</select
					></label
				>
				<label class="hp-field"
					><span>From date</span><Input
						type="date"
						size="sm"
						label="From date"
						bind:value={createdFrom}
					/></label
				>
				<label class="hp-field"
					><span>To date</span><Input
						type="date"
						size="sm"
						label="To date"
						bind:value={createdTo}
					/></label
				>
				<div class="flex items-end">
					<Button
						variant="caps"
						size="sm"
						onclick={() => {
							origin = '';
							profileId = '';
							rootId = '';
							operationState = '';
							mode = '';
							createdFrom = '';
							createdTo = '';
						}}>Clear filters</Button
					>
				</div>
			</div>
		</section>

		{#if historyQuery.isLoading || settingsQuery.isLoading || policyQuery.isLoading}
			<div class="space-y-2" role="status" aria-label="Loading organization history">
				<div class="skeleton h-24 rounded-xl"></div>
				<div class="skeleton h-24 rounded-xl"></div>
			</div>
		{:else if historyQuery.isError || settingsQuery.isError || policyQuery.isError}
			<div class="alert alert-error" role="alert">Could not load organization history.</div>
		{:else if historyItems.length === 0}
			<div class="lm-bordered p-10 text-center">
				<FolderCog class="mx-auto h-8 w-8 text-muted" />
				<p class="lm-title mt-3 text-2xl">No matching management work</p>
				<p class="mt-1 text-sm text-muted">
					Change the filters or create a manual preview from the Organize files tab.
				</p>
			</div>
		{:else}
			<div class="lm-bordered hp-table">
				<div class="hp-cols hp-cols--head" aria-hidden="true">
					<span>Date &amp; time</span><span>Operation</span><span>Profile &amp; scope</span><span
						>Result</span
					><span>Changes</span><span></span>
				</div>
				{#each historyItems as item (item.operation.id)}
					<a
						class="hp-cols hp-row"
						href={operationHref(
							item.operation.id,
							item.operation.state,
							item.operation.terminal_code ?? null
						)}
					>
						<span class="hp-when"
							><span>Started {formatDate(item.operation.created_at)}</span><small
								>{latestTimestampLabel(item.operation.state)}
								{formatDate(item.operation.updated_at)}</small
							></span
						>
						<span class="hp-mode"
							>{item.activation_preview
								? 'Activation dry run'
								: titleManagementValue(item.mode)}</span
						>
						<span class="min-w-0"
							><strong>{item.profile_name}</strong><small
								>{titleManagementValue(item.origin)} · {scopeLabel(
									isRecord(item.selection) ? item.selection : {},
									item.target_root_id ?? null
								)}</small
							></span
						>
						<span class="flex flex-wrap items-center gap-1"
							><StatusChip tone={stateTone(item.operation.state)}
								>{titleManagementValue(item.operation.state)}</StatusChip
							>{#if item.operation.state === 'ready' && item.expires_at}
								<span class="hp-expires">Expires {formatDate(item.expires_at)}</span>{/if}</span
						>
						<span class="flex flex-wrap gap-1"
							>{#if item.operation.state === 'ready'}
								{#if (item.eligible_count ?? 0) > 0}<span class="badge badge-success badge-sm"
										>{item.eligible_count} eligible</span
									>{/if}{#if (item.warning_count ?? 0) > 0}<span
										class="badge badge-warning badge-sm">{item.warning_count} warning</span
									>{/if}{#if (item.blocked_count ?? 0) > 0}<span class="badge badge-error badge-sm"
										>{item.blocked_count} blocked</span
									>{/if}
							{:else}
								{#if item.operation.succeeded_count}<span class="badge badge-success badge-sm"
										>{item.operation.succeeded_count} succeeded</span
									>{/if}{#if item.operation.failed_count}<span class="badge badge-error badge-sm"
										>{item.operation.failed_count} failed</span
									>{/if}{#if item.operation.skipped_count}<span class="badge badge-warning badge-sm"
										>{item.operation.skipped_count} skipped</span
									>{/if}
							{/if}</span
						>
						<ChevronRight class="hp-go size-5" aria-hidden="true" />
					</a>
				{/each}
			</div>
			{#if historyQuery.hasNextPage}<Button
					variant="outline"
					class="w-full"
					loading={historyQuery.isFetchingNextPage}
					onclick={() => void historyQuery.fetchNextPage()}>Load older history</Button
				>{/if}
		{/if}
	</main>
</div>

<style>
	.hp-filters {
		padding: 1.25rem;
		background-color: var(--color-base-200);
	}
	.hp-field {
		display: grid;
		gap: 0.375rem;
		font-size: 0.8125rem;
		font-weight: 600;
	}

	.hp-table {
		overflow: hidden;
		background-color: var(--color-base-200);
	}
	.hp-cols {
		display: grid;
		grid-template-columns: 12rem 9rem minmax(0, 1fr) 11rem 13rem 1.25rem;
		align-items: center;
		gap: 1rem;
		padding: 0.875rem 1.25rem;
	}
	.hp-cols--head {
		padding-block: 0.625rem;
		background-color: var(--dn-paper);
		color: var(--dn-paper-content);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	:global([data-theme='light']) .hp-cols--head {
		background-color: var(--dn-selected);
		color: var(--color-base-content);
	}
	.hp-row {
		border-top: 1px solid var(--dn-line);
		font-size: 0.875rem;
		transition: background-color 150ms;
	}
	.hp-cols--head + .hp-row {
		border-top: 0;
	}
	.hp-row:hover {
		background-color: var(--dn-selected);
	}
	.hp-row:focus-visible {
		outline: 2px solid var(--dn-focus);
		outline-offset: -2px;
	}
	.hp-row small {
		display: block;
		margin-top: 0.125rem;
		color: var(--dn-muted);
	}
	.hp-when {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
	}
	.hp-mode {
		font-weight: 600;
	}
	.hp-expires {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--dn-muted);
	}

	@media (max-width: 1023px) {
		.hp-cols--head {
			display: none;
		}
		.hp-cols {
			grid-template-columns: minmax(0, 1fr) auto;
			gap: 0.375rem 1rem;
		}
		.hp-cols > :global(*) {
			grid-column: 1;
		}
		.hp-cols > :global(.hp-go) {
			grid-column: 2;
			grid-row: 1 / span 5;
		}
		.hp-row {
			border-top-width: 1px;
		}
		.hp-cols--head + .hp-row {
			border-top: 0;
		}
	}
</style>
