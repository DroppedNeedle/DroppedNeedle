<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import { Info } from 'lucide-svelte';

	type YouTubeConnectionSettings = {
		api_key: string;
		enabled: boolean;
		api_enabled: boolean;
		daily_quota_limit: number;
	};

	type YouTubeTestResult = { valid: boolean; message: string };
	type YouTubeSettingsForm = ReturnType<typeof createSettingsForm<YouTubeConnectionSettings>> & {
		testResult: YouTubeTestResult | null;
	};

	const form = createSettingsForm<YouTubeConnectionSettings>({
		loadEndpoint: SETTINGS_ENDPOINTS.youtube(),
		saveEndpoint: SETTINGS_ENDPOINTS.youtube(),
		testEndpoint: SETTINGS_ENDPOINTS.youtubeVerify(),
		enabledField: 'enabled',
		refreshIntegration: true
	}) as YouTubeSettingsForm;

	let showKey = $state(false);
	let wasApiAlreadyEnabled = $state(false);

	export async function load() {
		await form.load();
		wasApiAlreadyEnabled = form.data?.api_enabled ?? false;
	}

	async function save() {
		const saved = await form.save();
		if (saved) {
			wasApiAlreadyEnabled = form.data?.api_enabled ?? false;
		}
	}

	async function test() {
		await form.test();
	}

	$effect(() => {
		load();
	});

	onDestroy(() => form.cleanup());
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl">YouTube</h2>
		<p class="text-base-content/70 mb-4">
			Enable YouTube features across the app: manage album links, search YouTube, and optionally
			enable the API for automatic link generation.
		</p>

		{#if form.loading}
			<div class="flex justify-center items-center py-12">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<div class="space-y-6">
				<div class="space-y-2">
					<div class="form-control">
						<label class="label cursor-pointer justify-start gap-4">
							<input
								type="checkbox"
								bind:checked={form.data.enabled}
								class="toggle toggle-primary"
							/>
							<div>
								<span class="label-text font-medium">Enable YouTube</span>
								<p class="text-xs text-base-content/50">
									Show YouTube features in the sidebar, album pages, and YouTube links page. Manual
									links and YouTube search work without an API key.
								</p>
							</div>
						</label>
					</div>
				</div>

				<div class="divider my-1"></div>

				<fieldset
					disabled={!form.data.enabled}
					class="space-y-4"
					class:opacity-50={!form.data.enabled}
				>
					<div class="flex items-center gap-2">
						<h3 class="text-lg font-semibold">YouTube API</h3>
						<div class="badge badge-sm badge-ghost">Optional</div>
					</div>
					<p class="text-xs text-base-content/50 -mt-2">
						Enable automatic link generation using the YouTube Data API. Get a free key from the
						<a
							href="https://console.cloud.google.com/apis/library/youtube.googleapis.com"
							target="_blank"
							rel="noopener noreferrer"
							class="link link-primary">Google Cloud Console</a
						>.
					</p>

					<div class="form-control w-full">
						<label class="label" for="yt-api-key">
							<span class="label-text">API Key</span>
						</label>
						<div class="join w-full">
							<input
								id="yt-api-key"
								type={showKey ? 'text' : 'password'}
								bind:value={form.data.api_key}
								class="input input-bordered join-item flex-1"
								placeholder="Your YouTube Data API v3 key"
							/>
							<button type="button" class="btn join-item" onclick={() => (showKey = !showKey)}>
								{showKey ? 'Hide' : 'Show'}
							</button>
						</div>
						<label class="label" for="yt-api-key">
							<span class="label-text-alt text-base-content/50"
								>Enable YouTube Data API v3, then create an API key in Credentials</span
							>
						</label>
					</div>

					{#if form.testResult}
						<div
							class="alert"
							class:alert-success={form.testResult.valid}
							class:alert-error={!form.testResult.valid}
						>
							<span>{form.testResult.message}</span>
						</div>
					{/if}

					<div class="form-control">
						<label class="label cursor-pointer justify-start gap-4">
							<input
								type="checkbox"
								bind:checked={form.data.api_enabled}
								class="toggle toggle-accent"
								disabled={!form.testResult?.valid && !wasApiAlreadyEnabled}
							/>
							<div>
								<span class="label-text font-medium">Enable YouTube API</span>
								<p class="text-xs text-base-content/50">
									{#if !form.testResult?.valid && !wasApiAlreadyEnabled}
										Test connection first to enable the API
									{:else}
										Auto-generate album and track links using the YouTube Data API (~100
										searches/day free quota)
									{/if}
								</p>
							</div>
						</label>
					</div>

					<div class="form-control w-full">
						<label class="label" for="yt-quota-limit">
							<span class="label-text">Daily Quota Limit</span>
						</label>
						<input
							id="yt-quota-limit"
							type="number"
							min="1"
							max="10000"
							bind:value={form.data.daily_quota_limit}
							class="input input-bordered w-32"
						/>
						<label class="label" for="yt-quota-limit">
							<span class="label-text-alt text-base-content/50">
								Maximum video lookups per day (default: 80)
							</span>
						</label>
					</div>

					<div class="flex justify-end">
						<button
							type="button"
							class="btn btn-ghost btn-sm"
							onclick={test}
							disabled={form.testing || !form.data.api_key}
						>
							{#if form.testing}
								<span class="loading loading-spinner loading-sm"></span>
							{/if}
							Test Connection
						</button>
					</div>
				</fieldset>

				{#if !form.data.enabled}
					<div class="alert alert-info">
						<Info class="h-4 w-4" />
						<span>Enable YouTube above to configure API settings.</span>
					</div>
				{/if}

				{#if form.message}
					<div
						class="alert"
						class:alert-success={form.messageType === 'success'}
						class:alert-error={form.messageType === 'error'}
					>
						<span>{form.message}</span>
					</div>
				{/if}

				<div class="flex justify-end pt-2">
					<button type="button" class="btn btn-primary" onclick={save} disabled={form.saving}>
						{#if form.saving}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Save Settings
					</button>
				</div>
			</div>
		{/if}
	</div>
</div>
