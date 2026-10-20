<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import {
		createSaveWrappedSettingsMutation,
		getWrappedSettingsQuery,
		type WrappedSettings
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import { Gift } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const section = createSettingsSection(
		getWrappedSettingsQuery(),
		createSaveWrappedSettingsMutation()
	);

	function generateKey(draft: WrappedSettings) {
		const bytes = crypto.getRandomValues(new Uint8Array(32));
		draft.api_key = Array.from(bytes)
			.map((b) => b.toString(16).padStart(2, '0'))
			.join('');
	}
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

		<QueryBoundary query={section} what="the Wrapped API key">
			{#snippet children(draft)}
				<div class="flex flex-col gap-4">
					<SecretField
						label="API key"
						mask={SECRET_MASK}
						placeholder="No key set"
						bind:value={draft.api_key}
					/>

					<Button variant="outline" size="sm" pill class="w-fit" onclick={() => generateKey(draft)}>
						Generate new key
					</Button>

					<SaveBar {...section.bar} saveLabel="Save key" />
				</div>
			{/snippet}
		</QueryBoundary>
	</div>
</Card>
