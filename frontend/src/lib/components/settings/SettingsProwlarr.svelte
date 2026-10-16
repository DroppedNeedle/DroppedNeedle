<script lang="ts">
	import { CircleCheck, CircleX, Radar } from 'lucide-svelte';

	import {
		getProwlarrConfigQuery,
		saveProwlarrConfigMutation,
		testProwlarrMutation
	} from '$lib/queries/downloads/ProwlarrQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { ProwlarrConnectionSettings, ProwlarrTestResult } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	const API_KEY_MASK = 'prowlarr****';

	const configQuery = getProwlarrConfigQuery();
	const save = saveProwlarrConfigMutation();
	const test = testProwlarrMutation();

	let draft = $state<ProwlarrConnectionSettings>({ enabled: false, url: '', api_key: '' });

	let testResult = $state<ProwlarrTestResult | null>(null);
	let saveError = $state<string | null>(null);
	let seeded = false;

	// Seed the form once from the masked config, then let the user edit freely.
	$effect(() => {
		const data = configQuery.data;
		if (data && !seeded) {
			draft = { enabled: data.enabled, url: data.url, api_key: data.api_key };
			seeded = true;
		}
	});

	async function runTest() {
		testResult = null;
		try {
			testResult = await test.mutateAsync({ ...draft });
		} catch {
			testResult = { valid: false, message: "Couldn't reach Prowlarr." };
		}
	}

	async function saveConnection() {
		saveError = null;
		try {
			await save.mutateAsync({ ...draft });
			toastStore.show({ message: 'Prowlarr connection saved', type: 'success' });
			// Re-mask the key in the form so it isn't shown after a save.
			if (draft.api_key && draft.api_key !== API_KEY_MASK) {
				draft = { ...draft, api_key: API_KEY_MASK };
			}
			testResult = null;
		} catch (e: unknown) {
			saveError = (e as { message?: string })?.message ?? 'Could not save the connection';
		}
	}
</script>

<section class="space-y-4">
	<Card variant="bordered" padding="none">
		<div>
			<div class="flex items-center gap-3 p-4">
				<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
					<Radar class="size-6 text-accent" aria-hidden="true" />
				</div>
				<div class="flex-1">
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
					mask={API_KEY_MASK}
					placeholder="Prowlarr → Settings → General → Security → API Key"
					bind:value={draft.api_key}
				/>

				<div class="flex flex-wrap items-center gap-3">
					<Button size="sm" onclick={runTest} disabled={test.isPending || !draft.url.trim()}>
						{test.isPending ? 'Testing…' : 'Test'}
					</Button>
					{#if testResult}
						<span
							class="flex items-center gap-1.5 text-sm"
							class:text-success={testResult.valid}
							class:text-error={!testResult.valid}
						>
							{#if testResult.valid}
								<CircleCheck class="size-4" aria-hidden="true" />
							{:else}
								<CircleX class="size-4" aria-hidden="true" />
							{/if}
							{testResult.message}
						</span>
					{/if}
					<div class="flex-1"></div>
					<Button
						variant="primary"
						size="sm"
						onclick={saveConnection}
						disabled={save.isPending || !draft.url.trim()}
					>
						Save
					</Button>
				</div>

				{#if saveError}
					<div class="alert alert-error py-2 text-sm">{saveError}</div>
				{/if}
			</div>
		</div>
	</Card>
</section>
