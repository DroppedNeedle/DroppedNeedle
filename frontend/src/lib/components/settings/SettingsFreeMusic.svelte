<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { ExternalLink, Landmark } from 'lucide-svelte';
	import { API } from '$lib/constants';
	import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
	import { getPolicySummaryQuery } from '$lib/queries/downloads/PolicyQueries.svelte';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import type { FreeMusicSettings } from '$lib/types';
	import { withBasePath } from '$lib/utils/basePath';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';

	// Enabling this flips is_download_source_ready(), which gates the request buttons,
	// so the sidebar's integration status has to be re-read after a save.
	const form = createSettingsForm<FreeMusicSettings>({
		loadEndpoint: API.settingsFreeMusic(),
		saveEndpoint: API.settingsFreeMusic(),
		refreshIntegration: true,
		afterSave: () =>
			invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix }).catch(
				() => undefined
			)
	});

	onMount(() => form.load());

	// Read-only consumption of the global acquisition policy (spec): the Archive
	// flow no longer carries its own format choice - the shared order governs it.
	const policySummary = getPolicySummaryQuery();
	const policyStatus = $derived(policySummary.data?.quality_recipe_status ?? null);
	const policyNeedsAttention = $derived(
		policyStatus === 'invalid' || policyStatus === 'non_convertible'
	);
	const policyStatusMessage = $derived(
		policySummary.data?.quality_recipe_error ||
			(policyStatus === 'non_convertible'
				? 'The saved policy includes formats outside the supported FLAC and MP3 recipe.'
				: 'Choose a valid acquisition recipe before requesting music.')
	);
	onDestroy(() => form.cleanup());
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<div class="flex items-center gap-2">
			<Landmark class="h-5 w-5 text-primary" aria-hidden="true" />
			<h2 class="card-title">Free Music</h2>
		</div>
		<p class="text-sm text-base-content/60">
			Request an album and DroppedNeedle downloads it from the
			<a
				href="https://archive.org"
				target="_blank"
				rel="noopener noreferrer"
				class="link link-hover inline-flex items-center gap-0.5"
			>
				Internet Archive <ExternalLink class="h-3 w-3" aria-hidden="true" />
			</a>
			when it is there. Only music carrying a Creative Commons or public-domain licence is offered, and
			the licence is shown before anything downloads. No account, no API key.
		</p>

		{#if form.loading}
			<div class="space-y-3 pt-2">
				<div class="skeleton h-12 w-full rounded-xl"></div>
				<div class="skeleton h-12 w-full rounded-xl"></div>
			</div>
		{:else if form.data}
			<div class="form-control pt-2">
				<label class="flex cursor-pointer items-start gap-3">
					<input type="checkbox" class="toggle toggle-primary" bind:checked={form.data.enabled} />
					<div>
						<span class="label-text font-medium">Enabled</span>
						<p class="text-xs text-base-content/50">
							Turn this off and requests need a download client, or a purchase you drop in yourself.
						</p>
					</div>
				</label>
			</div>

			<div class="form-control pt-2">
				<span class="label-text font-medium">Quality</span>
				{#if policyNeedsAttention}
					<div class="alert alert-warning mt-2 items-start" role="alert">
						<div class="min-w-0">
							<strong>Acquisition quality policy needs attention</strong>
							<p class="text-sm">{policyStatusMessage}</p>
						</div>
						<a
							href={withBasePath('/settings?tab=download-client')}
							class="btn btn-warning btn-outline btn-sm min-h-11 shrink-0"
						>
							Open acquisition policy
						</a>
					</div>
				{:else if policySummary.isError}
					<div class="alert alert-warning mt-2" role="alert">
						<p>Could not load the acquisition policy summary.</p>
					</div>
				{:else}
					<p class="text-sm text-base-content/70" data-testid="free-music-policy-summary">
						{policySummary.data?.summary || 'Use the admin policy summary'}
					</p>
				{/if}
				<p class="mt-1 text-xs text-base-content/50">
					Archive requests follow the server's download-quality order, so no separate format is
					chosen here.
				</p>
			</div>

			<div class="card-actions justify-end pt-2">
				<button class="btn btn-primary btn-sm" onclick={() => form.save()} disabled={form.saving}>
					{form.saving ? 'Saving…' : 'Save'}
				</button>
			</div>

			{#if form.message}
				<p class="text-sm {form.messageType === 'error' ? 'text-error' : 'text-success'}">
					{form.message}
				</p>
			{/if}
		{/if}
	</div>
</div>
