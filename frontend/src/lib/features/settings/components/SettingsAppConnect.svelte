<script lang="ts">
	import { Plus, Smartphone, Trash2 } from 'lucide-svelte';

	import {
		createSaveAppConnectSettingsMutation,
		getAppConnectSettingsQuery
	} from '$lib/features/profile/queries/connect-apps.svelte';
	import type {
		AppConnectAccess,
		AppConnectSettings
	} from '$lib/features/profile/queries/connect-apps.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextAreaField from '$lib/ui/TextAreaField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	// The server's limit on listed apps.
	const MAX_APPS = 8;
	const accessOptions: { value: AppConnectAccess; label: string }[] = [
		{ value: 'app_password', label: 'App-password (OpenSubsonic and Jellyfin)' },
		{ value: 'device_session', label: 'Device session (DroppedNeedle API)' }
	];
	const accessHelp: Record<AppConnectAccess, string> = {
		app_password: 'An app-password works with the compatibility APIs enabled on this server.',
		device_session:
			'A device session can use the DroppedNeedle API with the signed-in user’s permissions, including administrator actions for admins.'
	};

	const section = createSettingsSection(
		getAppConnectSettingsQuery(),
		createSaveAppConnectSettingsMutation(),
		{
			normalize: (draft: AppConnectSettings) => ({
				clients: draft.clients.map((client) => ({
					client_id: client.client_id.trim(),
					name: client.name.trim(),
					credential_kind: client.credential_kind ?? 'app_password',
					redirect_uris: client.redirect_uris.map((uri) => uri.trim()).filter(Boolean)
				}))
			})
		}
	);
</script>

<Card variant="bordered">
	{#snippet header()}
		<CardTitle as="h3" class="text-base">
			<Smartphone class="h-4 w-4" aria-hidden="true" />
			Sign-in from apps
		</CardTitle>
	{/snippet}
	<p class="text-sm text-muted">
		Only listed apps can request browser sign-in. Each person chooses whether to allow access. Leave
		the list empty to keep app sign-in off. Removing an app stops new sign-ins. Users can revoke
		existing device sessions under Profile, Sessions and app-passwords under Profile, Connect Apps.
	</p>
	<QueryBoundary query={section} what="the app sign-in settings">
		{#snippet children(draft)}
			<fieldset
				disabled={section.saving}
				aria-label="App registrations"
				class="mt-3 flex flex-col gap-4"
			>
				{#each draft.clients as client, index (index)}
					<Card variant="raised" padding="sm">
						<div class="flex flex-col gap-3">
							<div class="grid gap-3 sm:grid-cols-2">
								<TextField label="Client ID" mono bind:value={client.client_id} />
								<TextField label="Name" bind:value={client.name} />
							</div>
							<SelectField
								label="Access type"
								options={accessOptions}
								bind:value={
									() => client.credential_kind ?? 'app_password',
									(value) => (client.credential_kind = value)
								}
								help={accessHelp[client.credential_kind ?? 'app_password']}
							/>
							<TextAreaField
								label="Return addresses"
								help="One per line, matched exactly."
								mono
								rows={2}
								value={client.redirect_uris.join('\n')}
								oninput={(text) => (client.redirect_uris = text.split('\n'))}
							/>
							<div>
								<Button
									variant="danger-ghost"
									size="xs"
									icon={Trash2}
									aria-label={`Remove ${client.name || 'this app'}`}
									onclick={() => draft.clients.splice(index, 1)}>Remove</Button
								>
							</div>
						</div>
					</Card>
				{/each}
				<div>
					<Button
						size="sm"
						icon={Plus}
						disabled={draft.clients.length >= MAX_APPS}
						onclick={() =>
							draft.clients.push({
								client_id: '',
								name: '',
								redirect_uris: [],
								credential_kind: 'app_password'
							})}>Add app</Button
					>
				</div>
				<SaveBar {...section.bar} />
			</fieldset>
		{/snippet}
	</QueryBoundary>
</Card>
