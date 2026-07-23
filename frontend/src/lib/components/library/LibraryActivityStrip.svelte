<script lang="ts">
	import { AlertTriangle, ArrowRight, DatabaseZap } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryActivityQuery } from '$lib/queries/library/LibraryActivityQueries.svelte';
	import type { LibraryActivityResponse } from '$lib/queries/library/LibraryOperationsTypes';
	import LibraryWorkLane from './LibraryWorkLane.svelte';
	import { onMount } from 'svelte';

	interface Props {
		activityOverride?: LibraryActivityResponse | null;
		now?: number;
		userIdOverride?: string;
		adminOverride?: boolean;
	}

	let {
		activityOverride = null,
		now = undefined,
		userIdOverride = undefined,
		adminOverride = undefined
	}: Props = $props();
	let currentTime = $state(Date.now() / 1000);
	const effectiveNow = $derived(now ?? currentTime);

	onMount(() => {
		if (now !== undefined) return;
		const timer = window.setInterval(() => {
			currentTime = Date.now() / 1000;
		}, 60_000);
		return () => window.clearInterval(timer);
	});

	const userId = $derived(userIdOverride ?? authStore.user?.id);
	const isAdmin = $derived(adminOverride ?? authStore.isAdmin);
	const activityQuery = getLibraryActivityQuery(() => userId);
	const activity = $derived(activityOverride ?? activityQuery.data);
	const scan = $derived(activity?.items.find((item) => item.kind === 'scan'));
	const identification = $derived(activity?.items.find((item) => item.kind === 'identification'));
	const destination = $derived(isAdmin ? '/library/management#operations' : '/library');
	const identificationActive = $derived(
		Boolean(identification && identification.waiting_count > 0)
	);
	const foregroundActive = $derived(
		Boolean(identification && identification.foreground_operation_count > 0)
	);
	const failure = $derived(
		[scan, identification]
			.filter((item) => item?.failure_event_id && item.failure_at)
			.sort((left, right) => (right?.failure_at ?? 0) - (left?.failure_at ?? 0))[0]
	);
	const recentFailure = $derived(
		Boolean(
			failure?.failure_event_id &&
			failure.failure_at &&
			effectiveNow - failure.failure_at < 24 * 60 * 60
		)
	);
	const dismissalKey = $derived(
		userId && failure?.failure_event_id
			? `droppedneedle:library-failure:${userId}:${failure.failure_event_id}`
			: null
	);
	let dismissedKey = $state<string | null>(null);

	$effect(() => {
		if (!dismissalKey || typeof localStorage === 'undefined') {
			dismissedKey = null;
			return;
		}
		dismissedKey = localStorage.getItem(dismissalKey) === '1' ? dismissalKey : null;
	});

	const failureVisible = $derived(recentFailure && dismissedKey !== dismissalKey);
	const displayedScan = $derived(
		scan?.state === 'failed'
			? failureVisible && failure?.kind === 'scan'
				? scan
				: undefined
			: scan
	);
	const quietIdentification = $derived(
		Boolean(
			!displayedScan &&
			identificationActive &&
			!foregroundActive &&
			!failureVisible &&
			identification?.state === 'running' &&
			identification.started_at &&
			effectiveNow - identification.started_at >= 24 * 60 * 60
		)
	);
	const showStrip = $derived(
		Boolean(displayedScan || identificationActive || foregroundActive || failureVisible)
	);
	const accessibleName = $derived(
		[
			displayedScan
				? `Local files ${displayedScan.state}, ${displayedScan.processed} of ${displayedScan.total ?? 'an unknown total'}`
				: 'Local files idle',
			identification
				? `Identification ${identification.state}, ${identification.processed} complete and ${identification.waiting_count} waiting`
				: 'Identification idle'
		].join('. ')
	);
	const headline = $derived(
		displayedScan
			? displayedScan.state === 'discovering'
				? 'Counting local files'
				: displayedScan.state === 'indexing'
					? 'Indexing local files'
					: displayedScan.state === 'reconciling'
						? 'Reconciling library'
						: displayedScan.state === 'pausing'
							? 'Pausing after the current file...'
							: displayedScan.state === 'paused'
								? 'Local library update paused'
								: displayedScan.state === 'stopping'
									? 'Stopping after the current file...'
									: displayedScan.state === 'failed'
										? 'Local library update failed'
										: 'Scan in progress'
			: identification?.state === 'pausing'
				? 'Pausing identification...'
				: identification?.state === 'paused'
					? 'Identification paused'
					: identification?.state === 'failed'
						? 'Library identification failed'
						: foregroundActive
							? 'Administrative library work in progress'
							: 'Library identification in progress'
	);

	function dismissFailure(): void {
		if (!dismissalKey) return;
		dismissedKey = dismissalKey;
		try {
			localStorage.setItem(dismissalKey, '1');
		} catch {
			// The current page still honours dismissal when browser storage is unavailable.
		}
	}
</script>

{#if showStrip}
	<div class="library-activity-shell" data-testid="library-activity-strip">
		<span class="sr-only" aria-live="polite" aria-atomic="true">{headline}</span>
		{#if failureVisible}
			<div class="library-activity-failure" role="status">
				<AlertTriangle class="h-4 w-4 shrink-0" aria-hidden="true" />
				<span class="min-w-0 flex-1"
					>{failure?.kind === 'scan'
						? 'Local library update needs attention'
						: 'Library identification needs attention'}</span
				>
				<button
					type="button"
					class="btn btn-ghost btn-xs"
					onclick={dismissFailure}
					aria-label="Dismiss library failure">Dismiss</button
				>
			</div>
		{/if}

		<a
			href={destination}
			class="library-activity-link"
			class:library-activity-link--quiet={quietIdentification}
			aria-label={accessibleName}
		>
			{#if quietIdentification && identification}
				<DatabaseZap class="h-4 w-4 shrink-0 text-[var(--color-library-identify)]" />
				<span class="min-w-0 flex-1 truncate font-medium">Library identification continues</span>
				<span class="hidden text-xs text-base-content/60 sm:inline">
					{identification.processed.toLocaleString()} complete · {identification.waiting_count.toLocaleString()}
					waiting
				</span>
				<ArrowRight class="h-4 w-4 shrink-0" aria-hidden="true" />
			{:else}
				<div class="min-w-0 flex-1 space-y-1.5">
					<div class="flex items-center justify-between gap-3 text-xs">
						<span class="font-display font-semibold tracking-wide">Library work</span>
						<span class="truncate text-base-content/55">
							{headline}
						</span>
					</div>
					<LibraryWorkLane kind="scan" item={displayedScan} compact />
					<LibraryWorkLane kind="identification" item={identification} compact />
				</div>
				<ArrowRight class="h-4 w-4 shrink-0 text-base-content/50" aria-hidden="true" />
			{/if}
		</a>
	</div>
{/if}
