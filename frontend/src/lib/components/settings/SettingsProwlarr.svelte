<script lang="ts">
	import { Radar } from 'lucide-svelte';

	import {
		getProwlarrConfigQuery,
		saveProwlarrConfigMutation,
		testProwlarrMutation
	} from '$lib/queries/downloads/ProwlarrQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const test = testProwlarrMutation();

	const section = createSettingsSection(getProwlarrConfigQuery(), saveProwlarrConfigMutation(), {
		test: async (draft) => {
			const result = await test.mutateAsync(draft);
			return { passed: result.valid, message: result.message };
		}
	});
</script>

<section class="space-y-4">
	<Card variant="bordered" padding="none">
		<QueryBoundary query={section} what="the Prowlarr connection">
			{#snippet children(draft)}
				<div class="flex flex-wrap items-center gap-3 p-4">
					<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
						<Radar class="size-6 text-accent" aria-hidden="true" />
					</div>
					<div class="min-w-0 flex-1 basis-56">
						<h3 class="text-lg font-bold">Connection</h3>
						<p class="text-sm text-base-content/60">
							Searches run through Prowlarr when it is the selected backend. Prowlarr only finds
							releases - SABnzbd still downloads them.
						</p>
					</div>
					<label class="label cursor-pointer gap-2">
						<span>Enabled</span>
						<input
							type="checkbox"
							class="toggle toggle-sm toggle-primary"
							bind:checked={draft.enabled}
						/>
					</label>
				</div>

				<div class="space-y-4 border-t border-base-300 p-5">
					<TextField
						label="URL"
						mono
						placeholder="http://localhost:9696"
						help="Your Prowlarr address. A plain http:// LAN URL is fine."
						bind:value={draft.url}
					/>

					<SecretField
						label="API key"
						mask={SECRET_MASK}
						placeholder="Prowlarr → Settings → General → Security → API Key"
						bind:value={draft.api_key}
					/>

					<SaveBar {...section.bar} disabled={!draft.url.trim()} />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>
</section>
