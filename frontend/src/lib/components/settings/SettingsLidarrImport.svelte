<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { CircleCheck, CircleX, CloudDownload } from 'lucide-svelte';

	import { getLidarrImportConfigQuery } from '$lib/queries/lidarr-import/LidarrImportQueries.svelte';
	import {
		saveLidarrConfigMutation,
		testLidarrMutation
	} from '$lib/queries/lidarr-import/LidarrImportMutations.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { LidarrImportConnection, LidarrTestResult } from '$lib/queries/lidarr-import/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import LidarrImportSyncCard from './LidarrImportSyncCard.svelte';

	const configQuery = getLidarrImportConfigQuery();
	const save = saveLidarrConfigMutation();
	const test = testLidarrMutation();

	let draft = $state<LidarrImportConnection>({ url: '', api_key: '' });

	let testResult = $state<LidarrTestResult | null>(null);
	let saveError = $state<string | null>(null);
	let seeded = false;

	// Locked until a connection is saved. The masked config getter returns ''
	// api_key when nothing is stored, so a non-empty pair means configured.
	// The save mutation invalidates the config query, so this flips live after Save.
	const unlocked = $derived(
		(configQuery.data?.url ?? '').trim() !== '' && (configQuery.data?.api_key ?? '') !== ''
	);

	// Seed the form once from the masked config, then let the user edit freely.
	$effect(() => {
		const data = configQuery.data;
		if (data && !seeded) {
			draft = { url: data.url, api_key: data.api_key };
			seeded = true;
		}
	});

	async function runTest() {
		testResult = null;
		try {
			testResult = await test.mutateAsync({ ...draft });
		} catch {
			testResult = { valid: false, message: "Couldn't reach Lidarr." };
		}
	}

	async function saveConnection() {
		saveError = null;
		try {
			await save.mutateAsync({ ...draft });
			toastStore.show({ message: 'Lidarr connection saved', type: 'success' });
			// Re-mask the key in the form so it isn't shown after a save.
			if (draft.api_key && draft.api_key !== SECRET_MASK) {
				draft = { ...draft, api_key: SECRET_MASK };
			}
			testResult = null;
		} catch (e: unknown) {
			saveError = (e as { message?: string })?.message ?? 'Could not save the connection';
		}
	}
</script>

<section class="space-y-4">
	<header class="space-y-1">
		<h2 class="text-lg font-semibold">Lidarr Import</h2>
		<p class="max-w-prose text-sm text-base-content/70">
			Sync your monitored Lidarr artists into your follows, so you don't have to re-follow them by
			hand. Lidarr stays independent - this reads your monitored list, it doesn't manage anything.
		</p>
	</header>

	<Card variant="bordered" padding="none">
		<div>
			<div class="flex items-center gap-3 p-4">
				<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
					<CloudDownload class="size-6 text-accent" aria-hidden="true" />
				</div>
				<div>
					<h3 class="text-lg font-bold">Connection</h3>
					<p class="text-sm text-base-content/60">
						Save your Lidarr address and API key to unlock artist sync below.
					</p>
				</div>
			</div>

			<div class="space-y-4 border-t border-base-300 p-5">
				<TextField
					label="URL"
					mono
					placeholder="http://localhost:8686"
					help="Your Lidarr address. A plain http:// LAN URL is fine."
					bind:value={draft.url}
				/>

				<SecretField
					label="API key"
					mask={SECRET_MASK}
					placeholder="Lidarr → Settings → General → Security → API Key"
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

	<LidarrImportSyncCard {unlocked} />
</section>
