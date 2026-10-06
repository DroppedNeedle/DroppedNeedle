<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import type { WrappedSettingsResponse } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { Gift } from 'lucide-svelte';
	import { onMount, onDestroy } from 'svelte';

	const form = createSettingsForm<WrappedSettingsResponse>({
		loadEndpoint: SETTINGS_ENDPOINTS.wrapped(),
		saveEndpoint: SETTINGS_ENDPOINTS.wrapped()
	});

	let showKey = $state(false);

	function generateKey() {
		const bytes = crypto.getRandomValues(new Uint8Array(32));
		const key = Array.from(bytes)
			.map((b) => b.toString(16).padStart(2, '0'))
			.join('');
		if (form.data) form.data.api_key = key;
		showKey = true;
	}

	onMount(() => {
		form.load();
	});
	onDestroy(() => form.cleanup());
</script>

<div class="card border border-base-300/50 bg-base-200/60 backdrop-blur-sm">
	<div class="card-body gap-4">
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-purple-500/10 text-purple-400 ring-1 ring-purple-500/20"
			>
				<Gift class="h-5 w-5" />
			</div>
			<div>
				<h2 class="card-title text-2xl">Wrapped API</h2>
				<p class="text-sm text-base-content/60">
					Lets an external service (e.g. a newsletterr instance) pull year-in-review listening stats
					for your users.
				</p>
			</div>
		</div>

		<div class="rounded-xl border border-info/20 bg-info/5 p-3 text-sm text-base-content/70">
			Generate a key here, then paste the same value into the other service's settings. It's sent as <code
				>X-Wrapped-Api-Key</code
			>
			on requests to <code>/api/v3/wrapped/*</code>; those endpoints are disabled entirely while
			this is empty.
		</div>

		{#if form.loading}
			<div class="flex justify-center py-10">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<label class="form-control w-full">
				<span
					class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
				>
					API key
				</span>
				<div class="relative">
					<input
						type={showKey ? 'text' : 'password'}
						class="input input-soft w-full pr-16"
						bind:value={form.data.api_key}
						placeholder="No key set"
						autocomplete="off"
					/>
					<button
						type="button"
						class="btn btn-ghost btn-xs absolute right-2 top-1/2 -translate-y-1/2 rounded-full"
						onclick={() => (showKey = !showKey)}
					>
						{showKey ? 'Hide' : 'Show'}
					</button>
				</div>
			</label>

			<button
				type="button"
				class="btn btn-outline btn-sm w-fit gap-2 rounded-full"
				onclick={generateKey}
			>
				Generate new key
			</button>

			{#if form.message}
				<div
					class="alert"
					class:alert-success={form.messageType === 'success'}
					class:alert-error={form.messageType === 'error'}
				>
					<span>{form.message}</span>
				</div>
			{/if}

			<div class="flex justify-end pt-1">
				<button
					type="button"
					class="btn btn-primary glow-primary-soft gap-2 rounded-full"
					onclick={() => void form.save()}
					disabled={form.saving}
				>
					{#if form.saving}
						<span class="loading loading-spinner loading-sm"></span>
					{/if}
					Save key
				</button>
			</div>
		{:else if form.message}
			<div class="alert alert-error"><span>{form.message}</span></div>
		{/if}
	</div>
</div>
