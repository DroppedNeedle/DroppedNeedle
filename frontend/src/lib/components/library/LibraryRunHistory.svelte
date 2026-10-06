<script lang="ts">
	import { Download, FileClock, LoaderCircle } from 'lucide-svelte';
	import { api } from '$lib/api/client';
	import { API } from '$lib/constants';
	import { toastStore } from '$lib/stores/toast';
	import {
		getLibraryRunFailuresQuery,
		getLibraryRunHistoryQuery
	} from '$lib/queries/library/LibraryOperationQueries.svelte';
	import type { ScanRun } from '$lib/queries/library/LibraryOperationsTypes';
	import { tick } from 'svelte';

	const DEFAULT_VISIBLE_RUNS = 3;
	const historyQuery = getLibraryRunHistoryQuery();
	let showOlder = $state(false);
	const runs = $derived(historyQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const visibleRuns = $derived(showOlder ? runs : runs.slice(0, DEFAULT_VISIBLE_RUNS));
	const olderRunCount = $derived(Math.max(0, runs.length - DEFAULT_VISIBLE_RUNS));
	let detailRun = $state<ScanRun | null>(null);
	const failuresQuery = getLibraryRunFailuresQuery(() => detailRun?.id ?? null);
	const failureItems = $derived(failuresQuery.data?.pages.flatMap((page) => page.items) ?? []);
	let exportRun = $state<ScanRun | null>(null);
	let exporting = $state(false);
	let detailDialog: HTMLDialogElement;
	let detailHeading = $state<HTMLHeadingElement>();
	let detailOpener: HTMLButtonElement | null = null;
	let exportDialog: HTMLDialogElement;
	let exportHeading: HTMLHeadingElement;
	let exportOpener: HTMLButtonElement | null = null;

	function formatTime(value: number | null): string {
		return value ? new Date(value * 1000).toLocaleString() : '-';
	}

	function duration(run: ScanRun): string {
		if (!run.started_at || !run.terminal_at) return '-';
		const seconds = Math.max(0, Math.round(run.terminal_at - run.started_at));
		return seconds < 60 ? `${seconds}s` : `${Math.round(seconds / 60)}m`;
	}

	function phaseDuration(seconds: number): string {
		return seconds < 60 ? `${seconds.toFixed(1)}s` : `${(seconds / 60).toFixed(1)}m`;
	}

	function terminalReason(run: ScanRun): string {
		const labels: Record<string, string> = {
			COMPLETED: 'Finished normally',
			STOPPED: 'Stopped by an administrator',
			CANCELLED: 'Stopped by an administrator',
			POLICY_CHANGED: 'Stopped because library policy changed',
			SUPERSEDED_POLICY_CHANGED: 'Stopped because library policy changed',
			WALK_TIMEOUT: 'Stopped because a library folder stopped responding',
			FAILED: 'The scan could not finish'
		};
		return run.terminal_code
			? (labels[run.terminal_code] ?? 'Finished with a recorded status')
			: '-';
	}

	function openExport(
		run: ScanRun,
		event: MouseEvent & { currentTarget: HTMLButtonElement }
	): void {
		exportOpener = detailOpener ?? event.currentTarget;
		exportRun = run;
		if (detailDialog.open) detailDialog.close();
		exportDialog.showModal();
		exportHeading.focus();
	}

	async function openDetails(
		run: ScanRun,
		event: MouseEvent & { currentTarget: HTMLButtonElement }
	): Promise<void> {
		detailOpener = event.currentTarget;
		detailRun = run;
		await tick();
		detailDialog.showModal();
		detailHeading?.focus();
	}

	function closeDetails(): void {
		detailDialog.close();
	}

	function openDetailExport(event: MouseEvent & { currentTarget: HTMLButtonElement }): void {
		if (!detailRun) return;
		openExport(detailRun, event);
	}

	async function downloadDiagnostic(): Promise<void> {
		if (!exportRun) return;
		exporting = true;
		try {
			const response = await api.global.get<Response>(API.library.scanDiagnostics(exportRun.id), {
				raw: true
			});
			if (!response.ok) throw new Error('Diagnostic export failed');
			const blob = await response.blob();
			const disposition = response.headers.get('content-disposition') ?? '';
			const filename =
				disposition.match(/filename="([^"]+)"/)?.[1] ?? 'droppedneedle-library-run.json';
			const url = URL.createObjectURL(blob);
			const link = document.createElement('a');
			link.href = url;
			link.download = filename;
			link.click();
			URL.revokeObjectURL(url);
			exportDialog.close();
			toastStore.show({ message: 'Diagnostic report ready', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not prepare the diagnostic report', type: 'error' });
		} finally {
			exporting = false;
		}
	}
</script>

<section
	class="rounded-box border border-base-content/15 bg-base-200/40"
	aria-labelledby="run-history-title"
>
	<div class="flex items-center justify-between gap-3 border-b border-base-content/10 px-4 py-3">
		<h3 id="run-history-title" class="flex items-center gap-2 font-semibold">
			<FileClock class="h-4 w-4 text-primary" /> Recent runs
		</h3>
		<span class="text-xs text-base-content/50">Latest 50 retained</span>
	</div>
	{#if historyQuery.isLoading}
		<div class="space-y-2 p-4">
			<div class="skeleton h-12"></div>
			<div class="skeleton h-12"></div>
		</div>
	{:else if historyQuery.isError}
		<p class="p-4 text-sm text-error">Could not load recent library runs.</p>
	{:else if runs.length === 0}
		<p class="p-5 text-sm text-base-content/55">No library runs have finished yet.</p>
	{:else}
		<div class="hidden overflow-x-auto md:block">
			<table class="table table-sm">
				<thead
					><tr
						><th>Result</th><th>Trigger</th><th>Scope</th><th>Started</th><th>Finished</th><th
							>Counts</th
						><th>Duration</th><th><span class="sr-only">Actions</span></th></tr
					></thead
				>
				<tbody>
					{#each visibleRuns as run (run.id)}
						<tr>
							<td
								><span class="badge badge-ghost badge-sm">{run.state.replaceAll('_', ' ')}</span
								></td
							>
							<td>{run.trigger.replaceAll('_', ' ')}</td><td>{run.aggregate_scope}</td><td
								>{formatTime(run.started_at)}</td
							><td>{formatTime(run.terminal_at)}</td><td class="text-xs"
								>{(run.counters.changed_count ?? 0).toLocaleString()} changed · {(
									run.counters.errored_count ?? 0
								).toLocaleString()} errors</td
							><td>{duration(run)}</td>
							<td class="text-right">
								<button
									class="btn btn-ghost btn-xs"
									onclick={(event) => void openDetails(run, event)}>Details</button
								>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		<div class="divide-y divide-base-content/10 md:hidden">
			{#each visibleRuns as run (run.id)}
				<div class="grid grid-cols-2 gap-2 p-4 text-sm">
					<strong>{run.state.replaceAll('_', ' ')}</strong><span class="text-right"
						>{duration(run)}</span
					>
					<span class="text-base-content/55"
						>{run.trigger.replaceAll('_', ' ')} · {run.aggregate_scope}</span
					>
					<span class="text-right text-xs"
						>{(run.counters.changed_count ?? 0).toLocaleString()} changed</span
					>
					<button
						class="btn btn-ghost btn-xs col-span-2 justify-self-start"
						onclick={(event) => void openDetails(run, event)}>Details</button
					>
				</div>
			{/each}
		</div>
		{#if olderRunCount > 0}
			<div class="border-t border-base-content/10 p-2 text-center">
				<button class="btn btn-ghost btn-xs" onclick={() => (showOlder = !showOlder)}>
					{showOlder
						? `Show latest ${DEFAULT_VISIBLE_RUNS}`
						: `Show ${olderRunCount} older ${olderRunCount === 1 ? 'run' : 'runs'}`}
				</button>
			</div>
		{/if}
		{#if showOlder && historyQuery.hasNextPage}
			<div class="border-t border-base-content/10 p-3 text-center">
				<button
					class="btn btn-ghost btn-sm"
					disabled={historyQuery.isFetchingNextPage}
					onclick={() => void historyQuery.fetchNextPage()}
				>
					{#if historyQuery.isFetchingNextPage}<LoaderCircle class="h-4 w-4 animate-spin" />{/if} Load
					older runs
				</button>
			</div>
		{/if}
	{/if}
</section>

<dialog
	bind:this={detailDialog}
	class="modal"
	aria-labelledby="run-details-title"
	onclose={() => {
		detailRun = null;
		detailOpener?.focus();
	}}
>
	{#if detailRun}
		<div class="modal-box max-w-xl">
			<div class="flex items-start justify-between gap-4">
				<div>
					<h2
						bind:this={detailHeading}
						id="run-details-title"
						tabindex="-1"
						class="text-lg font-bold"
					>
						Run details
					</h2>
					<p class="mt-1 text-sm text-base-content/60">
						{detailRun.trigger.replaceAll('_', ' ')} · {detailRun.aggregate_scope} · {duration(
							detailRun
						)}
					</p>
				</div>
				<span class="badge badge-ghost badge-sm">{detailRun.state.replaceAll('_', ' ')}</span>
			</div>
			<p class="mt-4 text-sm">{terminalReason(detailRun)}</p>
			<dl class="mt-4 grid grid-cols-2 gap-3 text-sm sm:grid-cols-3">
				{#each Object.entries(detailRun.counters) as [name, count] (name)}
					<div class="rounded-box bg-base-200/60 p-3">
						<dt class="text-xs text-base-content/50">
							{name.replace('_count', '').replaceAll('_', ' ')}
						</dt>
						<dd class="mt-1 font-semibold">{count.toLocaleString()}</dd>
					</div>
				{/each}
			</dl>
			{#if Object.keys(detailRun.phase_timings).length}
				<h3 class="mt-4 text-sm font-semibold">Phase timings</h3>
				<ul class="mt-2 space-y-1 text-sm text-base-content/65">
					{#each Object.entries(detailRun.phase_timings) as [phase, seconds] (phase)}
						<li>{phase.replaceAll('_', ' ')} · {phaseDuration(seconds)}</li>
					{/each}
				</ul>
			{/if}
			{#if detailRun.state === 'failed' || (detailRun.counters['errored_count'] ?? 0) > 0}
				<h3 class="mt-4 text-sm font-semibold">Failed paths</h3>
				{#if failuresQuery.isLoading}
					<p class="mt-2 text-sm text-base-content/55">Loading failed paths…</p>
				{:else if failuresQuery.isError}
					<p class="mt-2 text-sm text-error">Could not load failed paths.</p>
				{:else if failureItems.length === 0}
					<p class="mt-2 text-sm text-base-content/55">No failed paths were recorded.</p>
				{:else}
					<ul class="mt-2 space-y-1 text-sm text-base-content/65">
						{#each failureItems as failure, index (index)}
							<li class="flex items-center justify-between gap-2">
								<span class="truncate">{failure.relative_path}</span>
								<span class="badge badge-error badge-sm shrink-0">{failure.failure_code}</span>
							</li>
						{/each}
					</ul>
					{#if failuresQuery.hasNextPage}
						<div class="mt-2 text-center">
							<button
								class="btn btn-ghost btn-sm"
								disabled={failuresQuery.isFetchingNextPage}
								onclick={() => failuresQuery.fetchNextPage()}
							>
								{#if failuresQuery.isFetchingNextPage}
									<span class="loading loading-spinner loading-xs"></span>
								{/if}
								Load more
							</button>
						</div>
					{/if}
				{/if}
			{/if}
			<div class="modal-action">
				{#if detailRun.diagnostics_available !== false}
					<button
						class="btn btn-outline btn-sm mr-auto"
						onclick={openDetailExport}
						aria-label={`Export diagnostics for run ${detailRun.id}`}
					>
						<Download class="h-3.5 w-3.5" /> Export diagnostics
					</button>
				{/if}
				<button class="btn btn-ghost" onclick={closeDetails}>Close</button>
			</div>
		</div>
	{/if}
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close run details">close</button>
	</form>
</dialog>

<dialog
	bind:this={exportDialog}
	class="modal"
	aria-labelledby="diagnostic-export-title"
	onclose={() => exportOpener?.focus()}
>
	<div class="modal-box max-w-lg">
		<h2
			bind:this={exportHeading}
			id="diagnostic-export-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			Export diagnostic report?
		</h2>
		<p class="mt-3 text-sm text-base-content/70">
			The report excludes credentials, raw provider responses, and full filesystem paths. Relative
			paths are replaced with non-reversible hashes.
		</p>
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => exportDialog.close()}>Cancel</button>
			<button
				class="btn btn-primary"
				disabled={exporting}
				onclick={() => void downloadDiagnostic()}
			>
				{#if exporting}<span class="loading loading-spinner loading-sm"></span>{/if} Export report
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close export dialog">close</button>
	</form>
</dialog>
