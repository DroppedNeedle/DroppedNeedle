<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import type { WrappedSettingsResponse } from '$lib/types';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { Gift } from 'lucide-svelte';
	import { onMount, onDestroy } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';

	const form = createSettingsForm<WrappedSettingsResponse>({
		loadEndpoint: SETTINGS_ENDPOINTS.wrapped(),
		saveEndpoint: SETTINGS_ENDPOINTS.wrapped()
	});

	function generateKey() {
		const bytes = crypto.getRandomValues(new Uint8Array(32));
		const key = Array.from(bytes)
			.map((b) => b.toString(16).padStart(2, '0'))
			.join('');
		if (form.data) form.data.api_key = key;
	}

	onMount(() => {
		form.load();
	});
	onDestroy(() => form.cleanup());
</script>

<Card variant="bordered">
	{#snippet header()}
		<div class="flex items-center gap-3">
			<div
				class="flex h-11 w-11 items-center justify-center rounded-xl bg-purple-500/10 text-purple-400 light:text-purple-700 ring-1 ring-purple-500/20"
			>
				<Gift class="h-5 w-5" />
			</div>
			<div>
				<CardTitle class="text-2xl">Wrapped API</CardTitle>
				<p class="text-sm text-base-content/60">
					Lets an external service (e.g. a newsletterr instance) pull year-in-review listening stats
					for your users.
				</p>
			</div>
		</div>
	{/snippet}
	<div class="mt-2 flex flex-col gap-4">
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
			<SecretField
				label="API key"
				mask={SECRET_MASK}
				placeholder="No key set"
				bind:value={form.data.api_key}
			/>

			<Button variant="outline" size="sm" pill class="w-fit" onclick={generateKey}>
				Generate new key
			</Button>

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
				<Button variant="primary" pill loading={form.saving} onclick={() => void form.save()}>
					Save key
				</Button>
			</div>
		{:else if form.message}
			<div class="alert alert-error"><span>{form.message}</span></div>
		{/if}
	</div>
</Card>
