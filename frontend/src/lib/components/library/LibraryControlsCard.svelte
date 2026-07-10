<script lang="ts">
	import { Radar, ArrowRight, CalendarClock, Play } from 'lucide-svelte';
	import {
		getLibrarySettingsQuery,
		getLibraryScanScheduleQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import LibraryScanScheduleControl from './LibraryScanScheduleControl.svelte';
	import LibraryScanButton from './LibraryScanButton.svelte';
	import LibraryForceRescanButton from './LibraryForceRescanButton.svelte';
	import { scanFrequencyLabel } from '$lib/utils/scanFrequency';
	import { formatLastUpdated } from '$lib/utils/formatting';
	import { authStore } from '$lib/stores/authStore.svelte';

	interface Props {
		lastScan?: Date | null;
	}
	let { lastScan = null }: Props = $props();

	const settingsQuery = getLibrarySettingsQuery(() => authStore.isAdmin);
	const scheduleQuery = getLibraryScanScheduleQuery(() => authStore.isAdmin);

	const paths = $derived(settingsQuery.data?.library_paths ?? []);
	const hasPath = $derived(paths.length > 0);
	const hasKey = $derived(!!settingsQuery.data?.acoustid_api_key);
	const isManual = $derived(scheduleQuery.data?.scan_frequency === 'manual');
	const freqLabel = $derived(
		scheduleQuery.data?.scan_frequency === 'daily'
			? `Daily at ${scheduleQuery.data?.daily_scan_time ?? '03:00'}`
			: scanFrequencyLabel(scheduleQuery.data?.scan_frequency)
	);
</script>

<div class="overflow-hidden rounded-2xl border border-base-content/8 bg-base-200/50">
	<div class="flex flex-wrap items-center gap-3 border-b border-base-content/8 px-5 py-4">
		<div
			class="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-accent/12 ring-1 ring-accent/25"
		>
			<Radar class="h-5 w-5 text-accent" />
		</div>
		<div class="min-w-0 flex-1">
			<div class="font-display font-semibold">Library controls</div>
			<div class="text-sm text-base-content/60">
				{isManual ? 'Auto-scan off' : `Auto-scan · ${freqLabel}`}
			</div>
		</div>
		<span class="font-mono text-xs tabular-nums text-base-content/50">
			Last scan {lastScan ? formatLastUpdated(lastScan) : 'never'}
		</span>
	</div>

	<div class="grid gap-6 p-5 sm:grid-cols-2">
		<section class="space-y-3">
			<h3
				class="flex items-center gap-2 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
			>
				<CalendarClock class="h-4 w-4 text-accent" /> Automatic scanning
			</h3>
			<LibraryScanScheduleControl />
		</section>

		<section class="space-y-3">
			<h3
				class="flex items-center gap-2 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
			>
				<Play class="h-4 w-4 text-accent" /> Manual scan
			</h3>
			<div class="flex flex-wrap items-center gap-2">
				<LibraryScanButton
					{hasPath}
					disabled={!hasPath}
					class="btn btn-primary btn-sm gap-1 rounded-full"
				/>
				<LibraryForceRescanButton
					{hasPath}
					class="btn btn-outline btn-error btn-sm gap-1 rounded-full"
				/>
			</div>
			{#if !hasPath}
				<p class="text-xs text-base-content/50">
					Add a library path in settings to enable scanning.
				</p>
			{/if}
		</section>
	</div>

	<div
		class="flex flex-wrap items-center justify-between gap-2 border-t border-base-content/8 px-5 py-3 text-xs text-base-content/55"
	>
		<span class="truncate font-mono tabular-nums">
			{paths.length} path{paths.length === 1 ? '' : 's'} · AcoustID {hasKey ? 'set' : 'not set'}
		</span>
		<a
			class="inline-flex items-center gap-1.5 rounded-full border border-base-content/10 px-3 py-1 transition-colors hover:border-base-content/25 hover:text-base-content"
			href="/settings?tab=library"
		>
			Manage library settings <ArrowRight class="h-3.5 w-3.5" />
		</a>
	</div>
</div>
