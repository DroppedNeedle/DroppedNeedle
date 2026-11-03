<script lang="ts">
	import { CircleAlert, CircleCheck } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import {
		createUpgradeFreshMutation,
		createUpgradeStartMutation,
		getUpgradePreflightQuery,
		type UpgradeCheck
	} from '$lib/queries/upgrade/UpgradeQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { FEATURE_LABELS, allCounts, headlineCounts, showAddress } from './upgradeText';

	const preflight = getUpgradePreflightQuery(() => true);
	const start = createUpgradeStartMutation();
	const fresh = createUpgradeFreshMutation();

	// Ticks belong to the check they were given for: a new check starts clear.
	let ticks = $state<{ at: number; indexes: number[] }>({ at: 0, indexes: [] });
	const ticked = $derived(ticks.at === preflight.dataUpdatedAt ? ticks.indexes : []);
	let confirmFresh = $state(false);
	let startError = $state<string | null>(null);

	function ready(check: UpgradeCheck): boolean {
		return (
			check.layout === 'v2' &&
			check.blockers.length === 0 &&
			check.warnings.every((_, index) => ticked.includes(index))
		);
	}

	function toggle(index: number, on: boolean) {
		const others = ticked.filter((tick) => tick !== index);
		ticks = { at: preflight.dataUpdatedAt, indexes: on ? [...others, index] : others };
	}

	async function upgrade(check: UpgradeCheck) {
		startError = null;
		try {
			await start.mutateAsync(check.warnings);
		} catch (cause) {
			startError = cause instanceof ApiError ? cause.message : 'Could not reach the server';
		}
	}

	function message(cause: unknown): string {
		return cause instanceof ApiError ? cause.message : 'Could not reach the server';
	}
</script>

<QueryBoundary query={preflight} what="the pre-flight">
	{#snippet loading()}
		<Card variant="bordered">
			<div class="flex items-center gap-3" aria-busy="true">
				<span class="loading loading-spinner loading-sm"></span>
				<span>Checking your v2 data. Nothing is changed.</span>
			</div>
		</Card>
	{/snippet}
	{#snippet failed(cause, retry)}
		<Card variant="bordered" title="The check did not run">
			<p class="text-sm text-base-content/70">{message(cause)}</p>
			{#snippet actions()}
				<Button onclick={retry}>Check again</Button>
			{/snippet}
		</Card>
	{/snippet}
	{#snippet children(result)}
		{#if result.check}
			{@const check = result.check}
			<Card variant="bordered" title="Before you upgrade">
				<div class="flex flex-col gap-5">
					{#if check.counts}
						<section aria-label="Found">
							<h3 class="mb-2 text-sm font-semibold">Found</h3>
							<dl class="grid grid-cols-2 gap-x-6 gap-y-1 text-sm sm:grid-cols-3">
								{#each headlineCounts(check.counts) as [label, count] (label)}
									<div class="flex justify-between gap-2">
										<dt class="text-base-content/70">{label}</dt>
										<dd class="font-mono">{count.toLocaleString()}</dd>
									</div>
								{/each}
							</dl>
							<details class="mt-2 text-sm">
								<summary class="cursor-pointer text-base-content/60">Everything</summary>
								<dl class="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-3">
									{#each allCounts(check.counts) as [label, count] (label)}
										<div class="flex justify-between gap-2">
											<dt class="text-base-content/70">{label}</dt>
											<dd class="font-mono">{count.toLocaleString()}</dd>
										</div>
									{/each}
								</dl>
							</details>
						</section>
					{/if}

					{#if check.blockers.length > 0}
						<section aria-label="Fix first" class="flex flex-col gap-2">
							<h3 class="text-sm font-semibold">Fix first</h3>
							{#each check.blockers as blocker (blocker.code)}
								<div class="alert alert-error items-start text-sm" role="alert">
									<CircleAlert class="size-4 shrink-0" />
									<div>
										<p>{blocker.message}</p>
										<p class="mt-1 font-medium">{blocker.fix}</p>
									</div>
								</div>
							{/each}
							<Button size="sm" class="self-start" onclick={() => void preflight.refetch()}>
								Check again
							</Button>
						</section>
					{/if}

					{#if check.warnings.length > 0}
						<section aria-label="Addresses that change" class="flex flex-col gap-2">
							<h3 class="text-sm font-semibold">Addresses that change</h3>
							{#each check.warnings as warning, index (index)}
								<div class="rounded-box border border-warning/40 bg-warning/5 p-3 text-sm">
									<p class="font-medium">{FEATURE_LABELS[warning.feature]}</p>
									<p class="mt-1 text-base-content/70">{warning.message}</p>
									{#if warning.old_address || warning.new_address}
										<dl class="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
											<dt class="text-base-content/60">Old</dt>
											<dd class="break-all font-mono text-xs">
												{showAddress(warning.old_address, window.location.origin)}
											</dd>
											<dt class="text-base-content/60">New</dt>
											<dd class="break-all font-mono text-xs">
												{showAddress(warning.new_address, window.location.origin)}
											</dd>
										</dl>
									{/if}
									<label class="mt-3 flex cursor-pointer items-center gap-2">
										<input
											type="checkbox"
											class="checkbox checkbox-sm checkbox-primary"
											checked={ticked.includes(index)}
											onchange={(event) => toggle(index, event.currentTarget.checked)}
										/>
										<span>
											{warning.old_address || warning.new_address ? 'I changed it' : 'I understand'}
										</span>
									</label>
								</div>
							{/each}
						</section>
					{/if}

					{#if check.layout !== 'v2' && check.blockers.length === 0}
						<p class="text-sm text-base-content/70">
							The data folders changed since this page opened. Reload the page.
						</p>
					{:else if check.blockers.length === 0}
						<p class="flex items-start gap-2 text-sm text-base-content/70">
							<CircleCheck class="mt-0.5 size-4 shrink-0 text-success" />
							Your v2 files move into a v2-backup folder, so you can go back.
						</p>
					{/if}

					{#if startError}
						<div class="alert alert-error py-2 text-sm" role="alert">{startError}</div>
					{/if}
				</div>
				{#snippet actions()}
					<div class="flex w-full flex-col-reverse gap-2 sm:flex-row sm:justify-between">
						<Button variant="ghost" onclick={() => (confirmFresh = true)}>
							Start fresh instead
						</Button>
						<Button
							variant="primary"
							disabled={!ready(check)}
							loading={start.isPending}
							onclick={() => void upgrade(check)}
						>
							Upgrade
						</Button>
					</div>
				{/snippet}
			</Card>
		{:else}
			<Card variant="bordered" title="The check did not finish">
				<p class="text-sm text-base-content/70">
					{result.failure?.message ?? 'It ended without an answer.'}
				</p>
				{#snippet actions()}
					<Button variant="ghost" onclick={() => (confirmFresh = true)}>Start fresh instead</Button>
					<Button onclick={() => void preflight.refetch()}>Check again</Button>
				{/snippet}
			</Card>
		{/if}
	{/snippet}
</QueryBoundary>

<ConfirmDialog
	bind:open={confirmFresh}
	title="Start fresh?"
	confirmLabel="Start fresh"
	pending={fresh.isPending}
	onconfirm={() => fresh.mutateAsync()}
>
	<ul class="list-disc space-y-1 pl-5 text-sm text-base-content/80">
		<li>Users, playlists, requests and history won't come across.</li>
		<li>Your music files aren't touched.</li>
		<li>v2's data is kept, so you can still go back.</li>
	</ul>
</ConfirmDialog>
