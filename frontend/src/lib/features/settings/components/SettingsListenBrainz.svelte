<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import { ExternalLink } from 'lucide-svelte';
	import { getListenBrainzConnectionQuery } from '$lib/features/settings/queries/listenbrainz.svelte';
	import {
		createSaveListenBrainzMutation,
		createVerifyListenBrainzMutation
	} from '$lib/features/settings/queries/listenbrainz.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const verify = createVerifyListenBrainzMutation();

	const section = createSettingsSection(
		getListenBrainzConnectionQuery(),
		createSaveListenBrainzMutation(),
		{
			test: async (draft) => {
				const result = await verify.mutateAsync(draft);
				return { passed: result.valid, message: result.message };
			}
		}
	);
</script>

<div class="flex flex-col gap-5">
	<SettingsPaneHeader title="ListenBrainz" description="Instance connection for scrobbling." />
	<Card variant="bordered">
		<QueryBoundary query={section} what="the ListenBrainz settings">
			{#snippet children(draft)}
				<div class="mt-2 flex flex-col gap-4">
					<ToggleRow label="Enable ListenBrainz" bind:checked={draft.enabled} />

					<TextField
						label="Username"
						placeholder="Your ListenBrainz username"
						bind:value={draft.username}
						error={section.fieldError('username', 'Username')}
					/>

					<SecretField
						label="User token"
						mask={SECRET_MASK}
						placeholder="User token"
						bind:value={draft.user_token}
						error={section.fieldError('user_token', 'User token')}
					/>

					<a
						href="https://listenbrainz.org/profile/"
						target="_blank"
						rel="noopener noreferrer"
						class="flex w-fit items-center gap-1 text-xs text-muted transition-colors hover:text-primary"
					>
						<ExternalLink class="h-3 w-3" /> Find your user token on your ListenBrainz profile
					</a>

					<SaveBar {...section.bar} testLabel="Test connection" />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>
</div>
