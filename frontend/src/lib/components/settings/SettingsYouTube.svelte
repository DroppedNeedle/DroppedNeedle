<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import {
		createSaveYouTubeSettingsMutation,
		createVerifyYouTubeMutation,
		getYouTubeSettingsQuery
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import { Info } from 'lucide-svelte';
	import Card from '$lib/ui/Card.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const verify = createVerifyYouTubeMutation();

	const section = createSettingsSection(
		getYouTubeSettingsQuery(),
		createSaveYouTubeSettingsMutation(),
		{
			refreshIntegration: true,
			test: async (draft) => {
				if (!draft.api_key) return { passed: false, message: 'Add an API key first.' };
				const result = await verify.mutateAsync(draft);
				return { passed: result.valid, message: result.message };
			}
		}
	);

	// The API switch stays locked until a test passes, unless already on.
	const apiLocked = $derived(!section.testPassed && !section.saved?.api_enabled);
</script>

<Card title="YouTube">
	<p class="text-base-content/70 mb-4">
		Enable YouTube features across the app: manage album links, search YouTube, and optionally
		enable the API for automatic link generation.
	</p>

	<QueryBoundary query={section} what="the YouTube settings">
		{#snippet children(draft)}
			<div class="space-y-6">
				<ToggleRow
					label="Enable YouTube"
					description="Show YouTube features in the sidebar, album pages, and YouTube links page. Manual links and YouTube search work without an API key."
					bind:checked={draft.enabled}
				/>

				<div class="divider my-1"></div>

				<fieldset disabled={!draft.enabled} class="space-y-4" class:opacity-50={!draft.enabled}>
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
						bind:value={draft.api_key}
					/>

					<ToggleRow
						label="Enable YouTube API"
						description={apiLocked
							? 'Test the connection first.'
							: 'Auto-generate album and track links using the YouTube Data API (~100 searches/day free quota)'}
						bind:checked={draft.api_enabled}
						disabled={apiLocked}
					/>

					<div class="max-w-xs">
						<NumberField
							label="Daily Quota Limit"
							help="Maximum video lookups per day (default: 80)"
							min={1}
							max={10000}
							bind:value={draft.daily_quota_limit}
						/>
					</div>
				</fieldset>

				{#if !draft.enabled}
					<div class="alert alert-info">
						<Info class="h-4 w-4" />
						<span>Enable YouTube above to configure API settings.</span>
					</div>
				{/if}

				<SaveBar {...section.bar} testLabel="Test connection" />
			</div>
		{/snippet}
	</QueryBoundary>
</Card>
