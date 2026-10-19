<script lang="ts">
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import {
		createSaveJellyfinSettingsMutation,
		createVerifyJellyfinMutation,
		getJellyfinSettingsQuery,
		type JellyfinVerifyResult
	} from '$lib/queries/settings/JellyfinSettingsQueries.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const verify = createVerifyJellyfinMutation();
	let users = $state<JellyfinVerifyResult['users']>([]);

	const section = createSettingsSection(
		getJellyfinSettingsQuery(),
		createSaveJellyfinSettingsMutation(),
		{
			refreshIntegration: true,
			test: async (draft) => {
				users = [];
				if (!draft.jellyfin_url || !draft.api_key) {
					return { passed: false, message: 'Add the URL and API key first.' };
				}
				const result = await verify.mutateAsync(draft);
				users = result.users;
				if (result.success && !draft.user_id && users.length > 0) draft.user_id = users[0].id;
				return { passed: result.success, message: result.message };
			}
		}
	);

	// The switches stay locked until a test passes, unless already on.
	const enableLocked = $derived(!section.testPassed && !section.saved?.enabled);
	const loginLocked = $derived(!section.testPassed && !section.saved?.login_enabled);
</script>

<Card variant="bordered" as="section">
	{#snippet header()}
		<div class="flex items-center gap-3">
			<div class="rounded-lg bg-primary/10 p-2">
				<JellyfinIcon class="h-5 w-5 text-primary" />
			</div>
			<div>
				<h2 class="font-semibold">Jellyfin</h2>
				<p class="text-sm text-base-content/60">Recently played and favorites from your server.</p>
			</div>
		</div>
	{/snippet}
	<QueryBoundary query={section} what="the Jellyfin settings">
		{#snippet children(draft)}
			<div class="mt-2 flex flex-col gap-5">
				<TextField
					label="Jellyfin URL"
					type="url"
					mono
					placeholder="http://localhost:8096"
					bind:value={draft.jellyfin_url}
				/>
				<div class="grid gap-4 md:grid-cols-2">
					<SecretField
						label="API key"
						mask={SECRET_MASK}
						placeholder="Your Jellyfin API key"
						help="Dashboard → API Keys → Create"
						bind:value={draft.api_key}
					/>
					{#if users.length > 0}
						<SelectField
							label="User"
							bind:value={draft.user_id}
							options={users.map((user) => ({ value: user.id, label: user.name }))}
						/>
					{:else}
						<TextField
							label="User ID"
							mono
							placeholder="Test to pick a user"
							help="Test the connection to choose from a list."
							bind:value={draft.user_id}
						/>
					{/if}
				</div>

				<div class="flex flex-col gap-4 border-t border-base-300 pt-5">
					<ToggleRow
						label="Use Jellyfin"
						description={enableLocked
							? 'Test the connection first.'
							: 'Show recently played and favorite artists on Home.'}
						bind:checked={draft.enabled}
						disabled={enableLocked}
					/>
					<ToggleRow
						label="Sign in with Jellyfin"
						description={loginLocked
							? 'Test the connection first.'
							: 'People sign in with their Jellyfin account.'}
						bind:checked={draft.login_enabled}
						disabled={loginLocked}
					/>
				</div>

				<SaveBar {...section.bar} testLabel="Test connection" />
			</div>
		{/snippet}
	</QueryBoundary>
</Card>
