<script lang="ts">
	import { CircleCheck, CircleX, DownloadCloud } from 'lucide-svelte';

	import { getLidarrImportConfigQuery } from '$lib/queries/lidarr-import/LidarrImportQueries.svelte';
	import {
		saveLidarrConfigMutation,
		testLidarrMutation
	} from '$lib/queries/lidarr-import/LidarrImportMutations.svelte';
	import { toastStore } from '$lib/stores/toast';
	import type { LidarrImportConnection, LidarrTestResult } from '$lib/queries/lidarr-import/types';

	const API_KEY_MASK = 'lidarr****';

	const configQuery = getLidarrImportConfigQuery();
	const save = saveLidarrConfigMutation();
	const test = testLidarrMutation();

	let draft = $state<LidarrImportConnection>({ url: '', api_key: '' });
	let showKey = $state(false);
	let testResult = $state<LidarrTestResult | null>(null);
	let saveError = $state<string | null>(null);
	let seeded = false;

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
	<header class="space-y-1">
		<h2 class="text-lg font-semibold">Lidarr Import</h2>
		<p class="max-w-prose text-sm text-base-content/70">
			One-time import of your monitored Lidarr artists as follows, so you don't have to re-follow
			them by hand. Lidarr stays independent - this reads your monitored list once, it doesn't
			manage anything.
		</p>
	</header>

	<div class="card border border-base-300 bg-base-200">
		<div class="card-body gap-0 p-0">
			<div class="flex items-center gap-3 p-4">
				<div class="grid size-12 place-items-center rounded-2xl bg-base-300/60">
					<DownloadCloud class="size-6 text-accent" aria-hidden="true" />
				</div>
				<div>
					<h3 class="text-lg font-bold">Connection</h3>
					<p class="text-sm text-base-content/60">
						Anyone can then import from Following → Your Artists.
					</p>
				</div>
			</div>

			<div class="space-y-4 border-t border-base-300 p-5">
				<div class="form-control">
					<label class="label" for="lidarr-url"><span class="label-text">URL</span></label>
					<input
						id="lidarr-url"
						class="input input-bordered input-sm w-full font-mono text-sm"
						placeholder="http://localhost:8686"
						bind:value={draft.url}
					/>
					<span class="label"
						><span class="label-text-alt text-base-content/50"
							>Your Lidarr address. A plain http:// LAN URL is fine.</span
						></span
					>
				</div>

				<div class="form-control">
					<label class="label" for="lidarr-key"><span class="label-text">API key</span></label>
					<div class="join w-full">
						<input
							id="lidarr-key"
							type={showKey ? 'text' : 'password'}
							class="input input-bordered input-sm join-item flex-1 font-mono text-sm"
							placeholder="Lidarr → Settings → General → Security → API Key"
							bind:value={draft.api_key}
						/>
						<button type="button" class="btn btn-sm join-item" onclick={() => (showKey = !showKey)}>
							{showKey ? 'Hide' : 'Show'}
						</button>
					</div>
				</div>

				<div class="flex flex-wrap items-center gap-3">
					<button
						type="button"
						class="btn btn-sm"
						onclick={runTest}
						disabled={test.isPending || !draft.url.trim()}
					>
						{test.isPending ? 'Testing…' : 'Test'}
					</button>
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
					<button
						type="button"
						class="btn btn-primary btn-sm"
						onclick={saveConnection}
						disabled={save.isPending || !draft.url.trim()}
					>
						Save
					</button>
				</div>

				{#if saveError}
					<div class="alert alert-error py-2 text-sm">{saveError}</div>
				{/if}
			</div>
		</div>
	</div>
</section>
