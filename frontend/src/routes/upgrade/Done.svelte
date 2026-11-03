<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';
	import {
		createUpgradeUndoMutation,
		type UpgradeResult
	} from '$lib/queries/upgrade/UpgradeQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import { allCounts, headlineCounts } from './upgradeText';

	interface Props {
		/** How the last run ended; none when the page opened after it. */
		result: UpgradeResult | null;
		continuing: boolean;
		continueError: string | null;
		oncontinue: () => void;
	}

	let { result, continuing, continueError, oncontinue }: Props = $props();

	const undo = createUpgradeUndoMutation();
	let confirmUndo = $state(false);

	const summary = $derived(result?.summary ?? null);
	const title = $derived(
		result?.outcome === 'started_fresh'
			? 'Started fresh'
			: result?.outcome === 'upgraded'
				? 'Upgrade done'
				: 'Ready'
	);
</script>

<Card variant="bordered" {title}>
	<div class="flex flex-col gap-5 text-sm">
		{#if result?.outcome === 'started_fresh'}
			<p class="text-base-content/70">
				Nothing came across from v2. Continue to create the first admin account, with the code from
				the server log.
			</p>
		{:else if !result || result.outcome !== 'upgraded'}
			<p class="text-base-content/70">The data folders hold v3 data.</p>
		{/if}

		{#if summary}
			{@const moved = headlineCounts(summary.moved)}
			<section aria-label="Moved">
				<h3 class="mb-2 font-semibold">Moved</h3>
				<dl class="grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-3">
					{#each moved as [label, count] (label)}
						<div class="flex justify-between gap-2">
							<dt class="text-base-content/70">{label}</dt>
							<dd class="font-mono">{count.toLocaleString()}</dd>
						</div>
					{/each}
				</dl>
				<details class="mt-2">
					<summary class="cursor-pointer text-base-content/60">Everything</summary>
					<dl class="mt-2 grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-3">
						{#each allCounts(summary.moved) as [label, count] (label)}
							<div class="flex justify-between gap-2">
								<dt class="text-base-content/70">{label}</dt>
								<dd class="font-mono">{count.toLocaleString()}</dd>
							</div>
						{/each}
					</dl>
				</details>
			</section>

			{#if summary.left_behind.length > 0}
				<section aria-label="Stayed in v2">
					<h3 class="mb-1 font-semibold">Stayed in v2</h3>
					<ul class="list-disc space-y-1 pl-5 text-base-content/80">
						{#each summary.left_behind as line, index (index)}
							<li>{line}</li>
						{/each}
					</ul>
				</section>
			{/if}

			{#if summary.notes.length > 0 || summary.needs_attention > 0 || summary.settings_defaulted.length > 0}
				<section aria-label="Worth a look">
					<h3 class="mb-1 font-semibold">Worth a look</h3>
					<ul class="list-disc space-y-1 pl-5 text-base-content/80">
						{#if summary.needs_attention > 0}
							<li>
								{summary.needs_attention.toLocaleString()}
								{summary.needs_attention === 1 ? 'record was' : 'records were'} left out or changed. The
								report lists each one.
							</li>
						{/if}
						{#each summary.notes as line, index (index)}
							<li>{line}</li>
						{/each}
						{#if summary.settings_defaulted.length > 0}
							<li>Back at v3's defaults: {summary.settings_defaulted.join(', ')}.</li>
						{/if}
					</ul>
				</section>
			{/if}

			<section aria-label="Full report">
				<h3 class="mb-1 font-semibold">Full report</h3>
				<p class="break-all font-mono text-xs">{summary.report}</p>
			</section>
		{/if}

		{#if result?.backup}
			<section aria-label="v2 backup">
				<h3 class="mb-1 font-semibold">Your v2 files</h3>
				<p class="mb-1 text-base-content/70">Kept so you can go back:</p>
				<p class="break-all font-mono text-xs">{result.backup.config}</p>
				<p class="break-all font-mono text-xs">{result.backup.cache}</p>
			</section>
		{/if}

		{#if continueError}
			<div class="alert alert-error py-2" role="alert">{continueError}</div>
		{/if}
	</div>
	{#snippet actions()}
		<div class="flex w-full flex-col-reverse gap-2 sm:flex-row sm:justify-between">
			{#if result?.backup}
				<Button variant="ghost" onclick={() => (confirmUndo = true)}>Go back to v2</Button>
			{:else}
				<span></span>
			{/if}
			<Button
				variant="primary"
				icon={ArrowRight}
				iconPosition="end"
				loading={continuing}
				onclick={oncontinue}
			>
				Continue
			</Button>
		</div>
	{/snippet}
</Card>

<ConfirmDialog
	bind:open={confirmUndo}
	title="Go back to v2?"
	message="The v3 data is deleted and v2's files go back where they were."
	confirmLabel="Go back to v2"
	tone="danger"
	pending={undo.isPending}
	onconfirm={() => undo.mutateAsync()}
/>
