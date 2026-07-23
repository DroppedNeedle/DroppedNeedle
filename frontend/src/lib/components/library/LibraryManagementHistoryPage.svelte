<script lang="ts">
	import { onMount } from 'svelte';
	import { ArrowRight, FolderCog, History, ShieldAlert } from 'lucide-svelte';

	import BackButton from '$lib/components/BackButton.svelte';
	import { getTargetLibrarySettingsQuery } from '$lib/queries/library/LibraryPolicyQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { createLibraryManagementEvents } from '$lib/queries/library-management/LibraryManagementEvents';
	import {
		getLibraryManagementOperationsQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
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

	onMount(() => {
		const events = createLibraryManagementEvents();
		events.start();
		return events.stop;
	});

	function dateBoundary(value: string, end: boolean): number | undefined {
		if (!value) return undefined;
		const suffix = end ? 'T23:59:59' : 'T00:00:00';
		const timestamp = new Date(`${value}${suffix}`).getTime();
		return Number.isFinite(timestamp) ? timestamp / 1000 : undefined;
	}

	function operationHref(jobId: string, value: string, terminalCode: string | null): string {
		return value === 'ready' || terminalCode === 'PREVIEW_DISCARDED'
			? `/library/management/previews/${encodeURIComponent(jobId)}`
			: `/library/management/operations/${encodeURIComponent(jobId)}`;
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
</script>

<svelte:head><title>Library Management history · DroppedNeedle</title></svelte:head>

<div class="management-preview-shell px-4 py-8 sm:px-6 lg:px-8">
	<main class="mx-auto max-w-6xl space-y-5">
		<BackButton fallback="/library/management#management-controls" />

		<header class="management-control-room p-5 sm:p-7">
			<div class="flex items-start gap-4">
				<div class="management-write-mark"><History class="h-6 w-6" /></div>
				<div>
					<p class="management-kicker">
						<ShieldAlert class="h-3.5 w-3.5" /> Administrator audit trail
					</p>
					<h1 class="mt-1 font-display text-2xl font-bold sm:text-3xl">
						Library Management history
					</h1>
					<p class="mt-2 text-sm text-base-content/60">
						Durable previews, writes, undo, baseline restores, and collision resolutions. Ordinary
						scans are not mixed into this history.
					</p>
				</div>
			</div>
		</header>

		<section
			class="management-operation-panel space-y-4"
			aria-labelledby="management-history-filters"
		>
			<h2 id="management-history-filters" class="font-semibold">Filter write-system history</h2>
			<div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
				<label class="grid gap-1 text-xs"
					><span>Origin</span><select
						class="select select-bordered select-sm bg-base-100"
						bind:value={origin}
						><option value="">All origins</option><option value="manual">Manual</option><option
							value="acquisition">Acquisition</option
						><option value="drop_import">Drop import</option><option value="scan_discovered"
							>Scan discovered</option
						></select
					></label
				>
				<label class="grid gap-1 text-xs"
					><span>Profile</span><select
						class="select select-bordered select-sm bg-base-100"
						bind:value={profileId}
						><option value="">All profiles</option
						>{#each settingsQuery.data?.profiles ?? [] as profile (profile.id)}<option
								value={profile.id}>{profile.name}</option
							>{/each}</select
					></label
				>
				<label class="grid gap-1 text-xs"
					><span>Root scope</span><select
						class="select select-bordered select-sm bg-base-100"
						bind:value={rootId}
						><option value="">All roots</option>{#each roots as root (root.id)}<option
								value={root.id}>{root.label}</option
							>{/each}</select
					></label
				>
				<label class="grid gap-1 text-xs"
					><span>State</span><select
						class="select select-bordered select-sm bg-base-100"
						bind:value={operationState}
						><option value="">All states</option
						>{#each ['queued', 'running', 'paused', 'ready', 'succeeded', 'failed', 'cancelled', 'stopped'] as option (option)}<option
								value={option}>{titleManagementValue(option)}</option
							>{/each}</select
					></label
				>
				<label class="grid gap-1 text-xs"
					><span>Mode</span><select
						class="select select-bordered select-sm bg-base-100"
						bind:value={mode}
						><option value="">All modes</option
						>{#each ['preview', 'apply', 'automatic_apply', 'undo', 'baseline_restore', 'duplicate_resolution'] as option (option)}<option
								value={option}>{titleManagementValue(option)}</option
							>{/each}</select
					></label
				>
				<label class="grid gap-1 text-xs"
					><span>From date</span><input
						type="date"
						class="input input-bordered input-sm bg-base-100"
						bind:value={createdFrom}
					/></label
				>
				<label class="grid gap-1 text-xs"
					><span>To date</span><input
						type="date"
						class="input input-bordered input-sm bg-base-100"
						bind:value={createdTo}
					/></label
				>
				<div class="flex items-end">
					<button
						class="btn btn-ghost btn-sm"
						onclick={() => {
							origin = '';
							profileId = '';
							rootId = '';
							operationState = '';
							mode = '';
							createdFrom = '';
							createdTo = '';
						}}>Clear filters</button
					>
				</div>
			</div>
		</section>

		{#if historyQuery.isLoading || settingsQuery.isLoading || policyQuery.isLoading}
			<div class="space-y-2">
				<div class="skeleton h-24 rounded-xl"></div>
				<div class="skeleton h-24 rounded-xl"></div>
			</div>
		{:else if historyQuery.isError || settingsQuery.isError || policyQuery.isError}
			<div class="alert alert-error">Could not load Library Management history.</div>
		{:else if historyItems.length === 0}
			<div class="rounded-2xl border border-dashed border-base-content/15 p-10 text-center">
				<FolderCog class="mx-auto h-7 w-7 text-base-content/35" />
				<p class="mt-2 font-semibold">No matching management work</p>
				<p class="text-sm text-base-content/50">
					Change the filters or create a manual preview from the Library control room.
				</p>
			</div>
		{:else}
			<div class="space-y-2">
				{#each historyItems as item (item.operation.id)}
					<a
						class="management-history-row"
						href={operationHref(
							item.operation.id,
							item.operation.state,
							item.operation.terminal_code
						)}
						><History class="h-4 w-4 text-library-manage" /><span class="min-w-0 flex-1"
							><span class="flex flex-wrap items-center gap-2"
								><strong>{item.profile_name}</strong><span class="badge badge-outline badge-sm"
									>{titleManagementValue(item.mode)}</span
								><span
									class="badge badge-sm {item.operation.state === 'failed'
										? 'badge-error'
										: item.operation.state === 'succeeded'
											? 'badge-success'
											: item.operation.state === 'ready'
												? 'badge-warning'
												: 'badge-outline'}">{titleManagementValue(item.operation.state)}</span
								></span
							><small
								>{titleManagementValue(item.origin)} · {scopeLabel(
									isRecord(item.selection) ? item.selection : {},
									item.target_root_id
								)} · {formatDate(item.operation.created_at)}</small
							><small
								>{item.operation.succeeded_count} succeeded · {item.operation.failed_count} failed · {item
									.operation.skipped_count} skipped</small
							></span
						><ArrowRight class="h-4 w-4" /></a
					>
				{/each}
			</div>
			{#if historyQuery.hasNextPage}<button
					class="btn btn-outline w-full"
					disabled={historyQuery.isFetchingNextPage}
					onclick={() => void historyQuery.fetchNextPage()}
					>{#if historyQuery.isFetchingNextPage}<span class="loading loading-spinner loading-sm"
						></span>{/if} Load older history</button
				>{/if}
		{/if}
	</main>
</div>
