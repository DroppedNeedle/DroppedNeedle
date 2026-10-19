<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import { Info } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

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

<Card title="YouTube">
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
			<ToggleRow
				label="Enable YouTube"
				description="Show YouTube features in the sidebar, album pages, and YouTube links page. Manual links and YouTube search work without an API key."
				bind:checked={form.data.enabled}
			/>

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

				<SecretField
					label="API Key"
					mask={SECRET_MASK}
					placeholder="Your YouTube Data API v3 key"
					help="Enable YouTube Data API v3, then create an API key in Credentials"
					bind:value={form.data.api_key}
				/>

				{#if form.testResult}
					<div
						class="alert"
						class:alert-success={form.testResult.valid}
						class:alert-error={!form.testResult.valid}
					>
						<span>{form.testResult.message}</span>
					</div>
				{/if}

				<ToggleRow
					label="Enable YouTube API"
					description={!form.testResult?.valid && !wasApiAlreadyEnabled
						? 'Test connection first to enable the API'
						: 'Auto-generate album and track links using the YouTube Data API (~100 searches/day free quota)'}
					bind:checked={form.data.api_enabled}
					disabled={!form.testResult?.valid && !wasApiAlreadyEnabled}
				/>

				<div class="max-w-xs">
					<NumberField
						label="Daily Quota Limit"
						help="Maximum video lookups per day (default: 80)"
						min={1}
						max={10000}
						bind:value={form.data.daily_quota_limit}
					/>
				</div>

				<div class="flex justify-end">
					<Button
						variant="ghost"
						size="sm"
						loading={form.testing}
						disabled={!form.data.api_key}
						onclick={test}
					>
						Test Connection
					</Button>
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
				<Button variant="primary" loading={form.saving} onclick={save}>Save Settings</Button>
			</div>
		</div>
	{/if}
</Card>
